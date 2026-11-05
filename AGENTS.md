# agent-reports

Self-hosted HTML report store. Reports live at `report://<project>/<slug>` for agents and at
`https://<your-host>/r/<project>/<slug>` for humans; the deployment target is yours (this repo assumes a
Traefik-fronted Kubernetes cluster, namespace `$AGENT_REPORTS_NAMESPACE`, default `agent-reports`).

Agents publish and edit reports through the omp plugin (`report://<project>/<slug>`); the human browses
them in a Svelte SPA with live websocket updates, split view, light/dark themes and 7-day share links.
Reports are stored verbatim as source documents, rendered inside a sandboxed iframe, and can carry a set
of interactive components (charts, diffs, before/after sliders, tabs, metrics).

## Layout

| Path | What |
|---|---|
| `server/` | Rust (axum + rusqlite) API, document renderer, websocket hub, static file server. Binary `agent-reports`. |
| `render/` | In-iframe runtime (`runtime.js`) + `report.css`. Built with Vite, served from `/_render/`. |
| `web/` | Svelte 5 + Vite SPA (shell, share shell, split view, theme). |
| `docs/guide.html` | The guide, seeded into the DB as report `agent-reports/guide` on start. |
| `plugin/` | omp plugin exposing `report://` URLs (hashline `edit` support). |
| `deploy/` | Kustomize: namespace, PVC, Deployment, Service, Issuer, Certificate, Middlewares, IngressRoute, secret templates. |
| `Dockerfile` | Multi-stage: bun builds `render/` + `web/`, Rust builds `server/`, distroless runtime. |

## Dev loop

```sh
bun install            # once per JS package (render/, web/, plugin/)
task dev               # render --watch, server on 127.0.0.1:8787, Vite on :5173
```

- **Vite (5173)** is the browser entry point. It proxies `/api` (including the websocket), `/s`, `/c` and
  `/_render` to `127.0.0.1:8787` and injects `X-Agent-Reports-Owner: dev`, so the server accepts requests as
  owner without Traefik.
- **Server (8787)** runs with `AGENT_REPORTS_DATA_DIR=../.data` (local SQLite + assets) and
  `AGENT_REPORTS_PUBLIC_URL=http://localhost:5173`.
- Data written during dev is throwaway; delete `.data/` to reset (the share/view-cap signing key lives
  there too).
- `task check` runs render/web/plugin lint+typecheck and `cargo fmt --check`, `clippy -D warnings`,
  `cargo test`.

## Deploy flow

Site values and secrets live in `.env` (mode 600, gitignored):

| Variable | Meaning |
|---|---|
| `AGENT_REPORTS_HOST` | public hostname, e.g. `reports.example.com` |
| `AGENT_REPORTS_PUBLIC_URL` | `https://$AGENT_REPORTS_HOST` |
| `AGENT_REPORTS_REGISTRY` | image registry prefix, e.g. `ghcr.io/you` |
| `ACME_EMAIL` | Let's Encrypt contact address |
| `AGENT_REPORTS_KUBE_CONTEXT` | kubectl context to apply to (empty = current) |
| `AGENT_REPORTS_NAMESPACE` | namespace every object is created in (default `agent-reports`) |
| `AGENT_REPORTS_PULL_SECRET` | image pull Secret in the namespace (default `registry-credentials`) |
| `AGENT_REPORTS_SESSION_TOKEN` | remember-cookie token (32 random bytes, hex) |
| `AGENT_REPORTS_OWNER_SECRET` | header the ingress stamps on authenticated requests |
| `AGENT_REPORTS_BASIC_AUTH_USER`, `AGENT_REPORTS_BASIC_AUTH_PASSWORD` | login |
| `AGENT_REPORTS_DEPLOY_DIR` | kustomize directory `task apply`/`deploy` render (default `deploy`); point it at a local overlay that lists `../deploy` as a resource to add site-specific objects without editing `deploy/` |

```sh
task secrets           # copy the pull secret + DNS-01 token secret into the namespace
task release           # image -> pin -> apply -> rollout status   (one-shot deploy)
task image             # buildx --platform linux/amd64 --push, tag <sha>-<YYYYMMDD-HHMM>
task pin TAG=<tag>     # only rewrite deploy/kustomization.yaml images.newTag
task deploy            # apply rendered manifests, wait for rollout
task apply             # apply only
task dns               # upsert the DNS-only A record for AGENT_REPORTS_HOST, idempotent
task logs              # follow server logs
task plugin:install    # bun install in plugin/ + `omp plugin link`
task plugin:login      # store AGENT_REPORTS_SESSION_TOKEN as the plugin `session` setting
```

`task apply`/`deploy` render `deploy/` with `kubectl kustomize`, substitute exactly
`$AGENT_REPORTS_SESSION_TOKEN $AGENT_REPORTS_OWNER_SECRET $AGENT_REPORTS_HTPASSWD $AGENT_REPORTS_HOST $AGENT_REPORTS_PUBLIC_URL
$AGENT_REPORTS_REGISTRY $AGENT_REPORTS_PULL_SECRET $AGENT_REPORTS_NAMESPACE $ACME_EMAIL` via `envsubst` (the htpasswd line is generated from
`.env` with `openssl passwd -apr1`), then `kubectl apply --context $AGENT_REPORTS_KUBE_CONTEXT -f -`. The explicit
variable list keeps unrelated `$` text (e.g. the `$apr1$` salt) intact. `task secrets` copies the image-pull
secret (`AGENT_REPORTS_PULL_SECRET`) and the DNS-01 API token secret into the namespace from `SOURCE_NS`.

The Deployment's `image: agent-reports` is a placeholder; `deploy/kustomization.yaml` rewrites it to
`${AGENT_REPORTS_REGISTRY}/agent-reports:<newTag>` (kustomize cannot match an image name that contains `${…}`).
Image tags are immutable; `images.newTag` is the pin. Rollback = `task pin`
with an older tag, then `task deploy`. The Deployment uses `Recreate` (SQLite on an RWO volume → never two
pods), runs as nonroot 65532 with a read-only root filesystem and `fsGroup: 65532` so `/data` is writable.

## Auth model

Traefik does all user auth; the app never authenticates users, and **the app never sets cookies**.

- `agent-reports-basic-auth` (basic auth, Secret `agent-reports-auth`) is the login.
- `agent-reports-remember` sets `Set-Cookie: __Host-agent-reports=<AGENT_REPORTS_SESSION_TOKEN>; Path=/; Max-Age=34560000;
  Secure; HttpOnly; SameSite=Lax` on successful (non-401) responses, refreshing the 400-day cookie.
- `agent-reports-owner` sets the request header `X-Agent-Reports-Owner: <AGENT_REPORTS_OWNER_SECRET>` on authenticated
  routes; the server requires a constant-time match for `/api/*` and the SPA shell.
- `agent-reports-public` deletes `X-Agent-Reports-Owner` on public routes.

IngressRoute priorities:

| Priority | Match | Middlewares |
|---|---|---|
| 100 | `/s/`, `/c/`, `/_render/`, `/assets/`, `/favicon.svg`, `/healthz` | `agent-reports-public` only |
| 50 | `Host(...)` + remember cookie | `agent-reports-owner`, `agent-reports-remember` |
| 10 | `Host($AGENT_REPORTS_HOST)` catch-all | `agent-reports-basic-auth`, `agent-reports-owner`, `agent-reports-remember` |

**Two hard rules**

1. **Never add `agent-reports-remember` (or basic auth) to the priority-100 public route.** It would hand the
   session cookie to any anonymous visitor of a share link.
2. **The application must never set cookies.** Cookie handling is Traefik-only; an app-set `Set-Cookie` on
   a public route would defeat rule 1.

Websockets (`/api/ws`, `/s/:token/ws`) need no special Traefik config; they pass through the same routes.

## DNS and TLS

- Point `AGENT_REPORTS_HOST` at the ingress with a **DNS-only** A record (`proxied=false` if Cloudflare: the
  proxy would need the origin certificate to match). `task dns` upserts it when `AGENT_REPORTS_ZONE` and
  `AGENT_REPORTS_HOST_IP` are set and `CF_API_TOKEN` is exported.
- Certificates: cert-manager Issuer `letsencrypt-dns` (Cloudflare DNS-01, Secret `cloudflare-api-token`)
  issues `agent-reports-tls` for `${AGENT_REPORTS_HOST}`. DNS-01 is used because a cluster that redirects HTTP to
  HTTPS breaks HTTP-01.
- Site-specific notes for the reference deployment live in `.omp/knowledge/` (gitignored).

## Token rotation (logout everywhere)

Rotating `AGENT_REPORTS_SESSION_TOKEN` in `.env` and running `task deploy` invalidates the remember cookie for
every browser and for the omp plugin (the cookie no longer matches the prio-50 route → basic auth again).
Re-run `task plugin:login` afterwards to store the new token for the plugin. Rotating
`AGENT_REPORTS_OWNER_SECRET` also requires `task deploy` and logs nobody out, but any non-Traefik caller using
the old owner header starts failing.
