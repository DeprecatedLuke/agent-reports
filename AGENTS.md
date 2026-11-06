# agent-reports

Self-hosted HTML report store. Reports live at `report://<project>/<slug>` for agents and at
`https://<your-host>/r/<project>/<slug>` for humans; the deployment target is yours (this repo assumes a
Traefik-fronted Kubernetes cluster, namespace `$AGENT_REPORTS_NAMESPACE`, default `agent-reports`).

Agents publish and edit reports through the omp plugin (`report://<project>/<slug>`); the human browses
them in a Svelte SPA with live websocket updates, split view, light/dark themes and 7-day share links.
Reports are stored verbatim as source documents, rendered inside a sandboxed iframe, and can carry a set
of interactive components (charts, diffs, before/after sliders, tabs, metrics).

## Layout

Two images, split by path: `agent-reports` (Rust, distroless) answers `/api/*` only; `agent-reports-web`
(nginx) serves the SPA shell, `/_render/` and `/assets/` from `web/dist` + `render/dist`. Traefik sends
`/api/*` to Rust and every other path to nginx; an app page only gets there after `/api/guard` accepts the session.

| Path | What |
|---|---|
| `server/` | Rust (axum + rusqlite) API under `/api/`, document renderer, websocket hub. Binary `agent-reports`. |
| `render/` | In-iframe runtime (`runtime.js`) + `report.css`. Built with Vite, served from `/_render/`. |
| `web/` | Svelte 5 + Vite SPA (shell, share shell, split view, theme); `web/nginx.conf` is the static server config. |
| `docs/guide.html` | The guide, seeded into the DB as report `agent-reports/guide` on start. |
| `plugin/` | omp plugin exposing `report://` URLs (hashline `edit` support). |
| `deploy/` | Kustomize: namespace, PVC, Deployments (`agent-reports`, `agent-reports-web`), Services, Issuer, Certificate, Middlewares, IngressRoute, secret templates. |
| `Dockerfile` | Multi-stage: bun builds `render/` + `web/`, Rust builds `server/`; two targets, `app` (distroless runtime) and `web-static` (nginx). |

## Dev loop

```sh
bun install            # once per JS package (render/, web/, plugin/)
task dev               # render --watch, server on 127.0.0.1:8787, Vite on :5173
```

- **Vite (5173)** is the browser entry point. It proxies `/api` (including the websocket) to `127.0.0.1:8787`
  and injects `Cookie: __Host-agent-reports=dev`, so the server accepts requests without a sign-in. It also
  serves `share.html` for `/s/<token>` and keeps `../render/dist` mounted at `/_render/`
  (`Access-Control-Allow-Origin: *`).
- **Server (8787)** runs with `AGENT_REPORTS_DATA_DIR=../.data` (local SQLite + assets),
  `AGENT_REPORTS_RENDER_DIR=../render/dist` (read for the runtime/CSS version hash and icon names) and
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
| `AGENT_REPORTS_SESSION_TOKEN` | value of the `__Host-agent-reports` cookie; exactly 64 lowercase hex chars (`openssl rand -hex 32`) |
| `AGENT_REPORTS_BASIC_AUTH_USER`, `AGENT_REPORTS_BASIC_AUTH_PASSWORD` | sign-in at `/api/login`; the password must be 1..72 bytes (bcrypt reads at most 72) |
| `AGENT_REPORTS_DEPLOY_DIR` | kustomize directory `task apply`/`deploy` render (default `deploy`); point it at a local overlay that lists `../deploy` as a resource to add site-specific objects without editing `deploy/` |

```sh
task secrets           # copy the pull secret + DNS-01 token secret into the namespace
task release           # image -> pin -> apply -> rollout status   (one-shot deploy)
task image             # buildx --platform linux/amd64 --push both targets, tag <sha>-<YYYYMMDD-HHMM>
task pin TAG=<tag>     # only rewrite deploy/kustomization.yaml images.newTag (both images)
task deploy            # apply rendered manifests, wait for both rollouts
task apply             # apply only
task dns               # upsert the DNS-only A record for AGENT_REPORTS_HOST, idempotent
task logs              # follow the API server logs
task logs:web          # follow the static web server logs
task plugin:install    # bun install in plugin/ + `omp plugin link`
task plugin:login      # store the public URL origin + session token as the plugin `session` setting
```

`task apply`/`deploy` render `deploy/` with `kubectl kustomize`, substitute exactly
`$AGENT_REPORTS_SESSION_TOKEN $AGENT_REPORTS_HTPASSWD $AGENT_REPORTS_HOST $AGENT_REPORTS_PUBLIC_URL
$AGENT_REPORTS_REGISTRY $AGENT_REPORTS_PULL_SECRET $AGENT_REPORTS_NAMESPACE $ACME_EMAIL` via `envsubst` (the htpasswd line is generated from
`.env` by piping the password to `bun` and `Bun.password.hash(..., { algorithm: "bcrypt", cost: 10 })`; Bun SHA-512-pre-hashes passwords over
72 bytes, which the server cannot verify, so `task apply` refuses a password longer than 72 bytes), then
`kubectl apply --context $AGENT_REPORTS_KUBE_CONTEXT -f -`. The explicit variable list keeps unrelated `$` text (e.g. the `$2b$10$` prefix of
the hash) intact. `task secrets` copies the image-pull
secret (`AGENT_REPORTS_PULL_SECRET`) and the DNS-01 API token secret into the namespace from `SOURCE_NS`.

Both Deployments' `image:` values are placeholders (`agent-reports`, `agent-reports-web`);
`deploy/kustomization.yaml` rewrites them to `${AGENT_REPORTS_REGISTRY}/agent-reports:<newTag>` and
`${AGENT_REPORTS_REGISTRY}/agent-reports-web:<newTag>` (kustomize cannot match an image name that contains
`${…}`). Image tags are immutable; `images.newTag` is the pin and `task pin` rewrites both entries with one tag.
Rollback = `task pin` with an older tag, then `task deploy`. The API Deployment uses `Recreate` (SQLite on an
RWO volume → never two pods), runs as nonroot 65532 with a read-only root filesystem and `fsGroup: 65532` so
`/data` is writable. The web Deployment is stateless: nginx-unprivileged on port 8080 as 101, read-only root
filesystem with an emptyDir at `/tmp`, default rolling update.

## Auth model

All user auth happens in the Rust server (`server/src/auth.rs`); Traefik only routes, caps `/api/login` and asks
`/api/guard` before it serves an app page.
**No secret value appears in any Traefik object** — the session token and the bcrypt htpasswd line live in Secret
`agent-reports-auth` and reach only the Rust Deployment env, never a Middleware, a route match or a Service.

Server env:

- `AGENT_REPORTS_SESSION_TOKEN` (required, non-empty) — the value of the `__Host-agent-reports` cookie.
- `AGENT_REPORTS_HTPASSWD` (optional, one `user:hash` line, bcrypt `$2a$`/`$2b$`/`$2y$`) — unset or empty disables
  password sign-in; the cookie keeps working.

Gate: public paths are `/api/healthz`, everything under `/api/s/` and `/api/c/`, and exactly `/api/login` and
`/api/guard`. Every other path needs the cookie in a `Cookie` header (pairs split on `;`, trimmed, compared as
SHA-256 digests in constant time). Failure is `401 {"error":"unauthorized"}` with no `WWW-Authenticate`, so a
background fetch never pops a browser prompt. Basic credentials are read by `/api/login` only.

`GET /api/guard` checks the cookie itself and is the Traefik `forwardAuth` target for app pages: `204` with a valid
cookie, otherwise `302` with `Location: /api/login?next=<percent-encoded X-Forwarded-Uri>` (run through the same
path rule as `next`; missing or unsafe gives `/`) and `Cache-Control: no-store`. Traefik hands that `302` to the
browser, so the SPA shell is never served to a browser that has no session. Any other method: `405`.

`GET /api/login?next=<path>`: a request that already carries a valid cookie gets `303 next`; a request whose Basic
credentials match the htpasswd user and hash (verified in `spawn_blocking`) gets `303 next` plus
`Set-Cookie: __Host-agent-reports=<session token>; Path=/; Max-Age=34560000; Secure; HttpOnly; SameSite=Lax`;
anything else gets `401` with `WWW-Authenticate: Basic realm="agent-reports", charset="UTF-8"` and a small
"Sign in required" page. `next` is used only if it starts with `/`, does not start with `//` or `/\`, and carries no
control characters; otherwise `/`. Any other method: `405`.

A browser holding the cookie sends it on every owner API call; a `401` sends it to
`/api/login?next=<current path + search + hash>`. The omp plugin signs in the same way (`loginWithBasic`, see
`plugin/README.md`) and keeps the cookie it reads from `Set-Cookie`.

The static build output is public: `/_render/`, `/assets/`, `/favicon.svg` and the `/s/` pages are build output of
this public repo and hold no data, so the priority-20 route carries no middleware (share viewers hold no cookie,
and the sandboxed report iframe sends none to `/_render/`). Every app page goes through the guard.

IngressRoute priorities:

| Priority | Match | Backend | Middlewares |
|---|---|---|---|
| 110 | `Host(...)` + `/api/login` | `agent-reports` | `agent-reports-login-limit` |
| 100 | `Host(...)` + `/api/` | `agent-reports` | none |
| 20 | `Host(...)` + `/s/`, `/_render/`, `/assets/`, `/favicon.svg` | `agent-reports-web` | none |
| 10 | `Host($AGENT_REPORTS_HOST)` catch-all (app pages) | `agent-reports-web` | `agent-reports-guard` |

`agent-reports-login-limit` (`rateLimit`: average 20, period 1m, burst 20) caps `/api/login`;
`agent-reports-guard` (`forwardAuth` to `GET /api/guard`, `authRequestHeaders: [Cookie]`) runs before nginx sees an
app-page request, so a browser without a session is sent to `/api/login` instead of getting the shell. The cluster's
Traefik Service uses `externalTrafficPolicy: Local`, so Traefik sees real client addresses and the default
per-source bucket is per client.

**Rule: keep secrets out of Traefik objects.** Everything in a Middleware or a route match is readable through the
CRD API (`kubectl get ingressroute -o yaml`, the Traefik dashboard, any cluster reader), so a cookie check or a
header the caller writes belongs in the server, never in the IngressRoute.

Websockets: `/api/ws` keeps its Origin check and is cookie-gated like every other non-public path (the plugin and the
SPA send the cookie on the upgrade); `/api/s/:token/ws` is public. Traefik needs no extra config for either.

## DNS and TLS

- Point `AGENT_REPORTS_HOST` at the ingress with a **DNS-only** A record (`proxied=false` if Cloudflare: the
  proxy would need the origin certificate to match). `task dns` upserts it when `AGENT_REPORTS_ZONE` and
  `AGENT_REPORTS_HOST_IP` are set and `CF_API_TOKEN` is exported.
- Certificates: cert-manager Issuer `letsencrypt-dns` (Cloudflare DNS-01, Secret `cloudflare-api-token`)
  issues `agent-reports-tls` for `${AGENT_REPORTS_HOST}`. DNS-01 is used because a cluster that redirects HTTP to
  HTTPS breaks HTTP-01.
- Site-specific notes for the reference deployment live in `.omp/knowledge/` (gitignored).

## Token rotation (logout everywhere)

Rotating `AGENT_REPORTS_SESSION_TOKEN` in `.env` and running `task deploy` invalidates the cookie for every browser
and for the omp plugin: the next API call gets `401`, the browser lands on `/api/login` and the plugin signs in with
`loginWithBasic` (or re-run `task plugin:login`: the plugin `session` setting is one string, the origin of
`AGENT_REPORTS_PUBLIC_URL` and the token separated by a single space, and the plugin sends the cookie only to that
origin). Rotating `AGENT_REPORTS_BASIC_AUTH_PASSWORD` also requires `task deploy` (the bcrypt htpasswd line is
regenerated) and logs nobody out: a browser or plugin that already holds the cookie keeps working.
