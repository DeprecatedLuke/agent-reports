mod api;
mod auth;
mod caps;
mod config;
mod db;
mod doc;
mod error;
mod events;
mod guide;
mod icons;
mod public;
mod source;
mod statics;
mod text;
mod ws;

use std::sync::Arc;

use anyhow::Context;
use axum::{Router, middleware, routing::get};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

/// Shared application state. Cheap to clone.
#[derive(Clone)]
pub struct App {
    pub cfg: Arc<config::Config>,
    pub db: db::Db,
    pub events: tokio::sync::broadcast::Sender<events::Event>,
    pub caps: Arc<caps::Caps>,
    pub version: Arc<str>,
    pub md: Arc<htmd::HtmlToMarkdown>,
    pub icons: Arc<icons::IconNames>,
}

fn build_router(app: App) -> Router {
    Router::new()
        .route("/api/healthz", get(api::healthz))
        .route("/api/me", get(api::me))
        .route("/api/projects", get(api::projects))
        .route("/api/reports", get(api::list_reports))
        .route(
            "/api/reports/{p}/{s}",
            get(api::get_report).delete(api::delete_report),
        )
        .route(
            "/api/reports/{p}/{s}/source",
            get(api::get_source).put(api::put_source),
        )
        .route("/api/reports/{p}/{s}/text", get(api::get_text))
        .route("/api/reports/{p}/{s}/body", get(api::get_body))
        .route("/api/reports/{p}/{s}/assets", get(api::list_assets))
        .route(
            "/api/reports/{p}/{s}/assets/{name}",
            get(api::get_asset)
                .put(api::put_asset)
                .delete(api::delete_asset),
        )
        .route(
            "/api/reports/{p}/{s}/shares",
            get(api::list_shares).post(api::create_share),
        )
        .route(
            "/api/reports/{p}/{s}/view-cap",
            axum::routing::post(api::view_cap),
        )
        .route(
            "/api/shares/{token}",
            axum::routing::delete(api::revoke_share),
        )
        .route("/api/ws", get(ws::global_ws))
        .route("/api/s/{token}/meta", get(public::share_meta))
        .route("/api/s/{token}/body", get(public::share_body))
        .route("/api/s/{token}/ws", get(ws::share_ws))
        .route("/api/c/{cap}", get(public::cap_redirect))
        .route("/api/c/{cap}/", get(public::cap_document))
        .route(
            "/api/c/{cap}/assets/{name}",
            get(public::cap_asset).options(public::preflight),
        )
        .fallback(api::not_found)
        .layer(middleware::from_fn_with_state(
            app.clone(),
            auth::owner_gate,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = config::Config::from_env().context("configuration")?;
    cfg.warn_missing_dirs();

    let db = db::Db::open(&cfg.data_dir).context("opening database")?;
    let signing = db
        .call(|c| caps::Caps::load_or_create(c))
        .await
        .context("loading view-cap signing key")?;
    let version = doc::render_version(&cfg.render_dir);
    tracing::info!(version = %version, "render asset version");
    let icon_names = icons::IconNames::load(&cfg.render_dir);

    let app = App {
        cfg: Arc::new(cfg.clone()),
        db,
        events: events::new_sender(),
        caps: Arc::new(signing),
        version: Arc::from(version.as_str()),
        md: Arc::new(text::converter()),
        icons: Arc::new(icon_names),
    };

    guide::seed(&app).await;

    let router = build_router(app);
    let listener = TcpListener::bind(cfg.bind.as_str())
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    tracing::info!(addr = %listener.local_addr()?, data = %cfg.data_dir.display(), "agent-reports server listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
