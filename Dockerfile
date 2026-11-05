# syntax=docker/dockerfile:1.7
#
# Multi-stage build for agent-reports (linux/amd64 in production):
#   render/ + web/  -> static bundles built with bun
#   server/         -> Rust release binary (axum + rusqlite)
#
# Two final images, both built from this file with `--target`:
#   app        distroless: the Rust API server, the render bundle it reads for
#              icons.json + cache-busting versions, and the guide. No web/dist.
#   web-static nginx: the SPA, the share shell and the report runtime.
# `app` is last so a plain `docker build` still yields the Rust image.

# ---------------------------------------------------------------------------
# render: in-iframe runtime (runtime.js, report.css)
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM oven/bun:1 AS render
WORKDIR /src/render
# Dependencies first so source edits do not invalidate the install layer.
COPY render/package.json render/bun.lock ./
RUN bun install --frozen-lockfile
COPY render/ ./
RUN bun run build

# ---------------------------------------------------------------------------
# web: Svelte SPA (index.html + share.html + assets)
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM oven/bun:1 AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
RUN bun run build

# ---------------------------------------------------------------------------
# server: Rust release build, dependency layer cached behind a stub main
# ---------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS server
WORKDIR /src
COPY server/Cargo.toml server/Cargo.lock ./
RUN mkdir -p src \
 && printf 'fn main() {}\n' > src/main.rs \
 && cargo build --release --locked
COPY server/ ./
RUN touch src/main.rs && cargo build --release --locked \
 && cp target/release/agent-reports /out-agent-reports

# ---------------------------------------------------------------------------
# web-static: everything static, served by nginx on 8080
# ---------------------------------------------------------------------------
FROM --platform=$TARGETPLATFORM nginxinc/nginx-unprivileged:alpine-slim AS web-static
# `auto` means one worker per node CPU, which outgrows the pod's memory limit;
# static files need two.
RUN sed -i 's/^worker_processes .*/worker_processes 2;/' /etc/nginx/nginx.conf
COPY web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=web /src/web/dist/ /usr/share/nginx/html/
COPY --from=render /src/render/dist/ /usr/share/nginx/render/
EXPOSE 8080

# ---------------------------------------------------------------------------
# app: Rust API server, distroless
# ---------------------------------------------------------------------------
FROM --platform=$TARGETPLATFORM gcr.io/distroless/cc-debian12:nonroot AS app
WORKDIR /app
COPY --from=server /out-agent-reports /app/agent-reports
COPY --from=render /src/render/dist/ /app/render/
COPY docs/guide.html /app/docs/guide.html
EXPOSE 8080
ENTRYPOINT ["/app/agent-reports"]
