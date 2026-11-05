# agent-reports omp plugin

Publishes and edits human-facing HTML reports on an agent-reports server from omp.
Reports are readable at `report://` URLs, editable in place with hashline `edit`, and
render as live HTML for the human.

## Install

```sh
cd plugin
bun install
omp plugin link "$PWD"          # symlinks this package into ~/.omp/plugins
omp plugin config set agent-reports url https://reports.example.com   # your deployment
```

The plugin loads from `package.json#omp.extensions` (`./src/index.ts`); no build step.

## Log in

```sh
/agent-reports login     # Basic auth once against /api/login; stores the __Host-agent-reports session cookie
/agent-reports status    # url, project, whether the stored cookie is accepted
/agent-reports logout    # clear the stored cookie
```

Non-interactive alternative (the cookie value is never echoed):

```sh
set -a; . ./.env; set +a
omp plugin config set agent-reports session "${AGENT_REPORTS_PUBLIC_URL%/} $AGENT_REPORTS_SESSION_TOKEN"
```

The setting is one string, `"<origin> <cookie>"`, so the stored cookie is bound to the origin it
was issued for — `omp plugin config set agent-reports session "https://reports.example.com <cookie>"`.
Requests carry the cookie only while `url` has that origin; if a project override changes `url` to
another host, the plugin sends no cookie and `/agent-reports status` says which origin the session
belongs to. A bare cookie value (from an older plugin version) counts as logged out, so log in again.

Settings live in `~/.omp/plugins/omp-plugins.lock.json` under `settings.agent-reports`
(`url`, secret `session`); project overrides in `.omp/plugin-overrides.json` win.

## URL scheme

| URL | Read | Write |
|---|---|---|
| `report://` / `report://list` | reports in the current project (`basename(cwd)` lowercased, other chars → `-`) | — |
| `report://list?project=<name>` / `report://list/<name>` | one project | — |
| `report://list?project=*` | every project, grouped | — |
| `report://guide` | the writing guide (`agent-reports/guide`) | — |
| `report://<project>/<slug>` | editable source with hashline anchors | `write` = PUT (`If-Match` known rev, else `If-None-Match: *`); `edit` = patch the local mirror, pushed back automatically |
| `report://<project>/<slug>.md` | server text version (immutable) | — |

Reads of a report source resolve through a local mirror (`${XDG_CACHE_HOME:-~/.cache}/omp-agent-reports/<project>/<slug>.md`)
because the host's hashline `edit` patches a real file; the mirror is refreshed from the
server on every read/`edit` and changes are PUT back after the tool result.

Errors are reported model-side: `412` → “report changed (or already exists) — read report://p/s again”,
`422` → validation problems with line numbers, `401` → “not logged in — run /agent-reports login”
(naming both origins when the stored session belongs to a different one).

## Tool

`report_upload {project, slug, path, name?}` uploads a local file (png/jpg/gif/webp/avif/svg,
csv, json, txt) to `…/assets/<name>` and returns `<img src="assets/<name>">` for images or the
`data.url: "assets/<name>"` hint for csv/json (use it inside an `<x-chart>` spec).

## Checks

```sh
bun run check   # tsc --noEmit
bun run lint    # biome check
```
