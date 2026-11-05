use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::{
    App, api, caps, db,
    db::{Db, ReportRow, ShareRow},
    doc,
    error::ApiError,
    source, statics,
};

/// Why a share/cap lookup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    NotFound,
    Gone,
    Internal,
}

fn missing_html(m: Missing) -> Response {
    match m {
        Missing::NotFound => statics::html_error(StatusCode::NOT_FOUND, "Share not found."),
        Missing::Gone => statics::html_error(
            StatusCode::GONE,
            "This share link has expired or was revoked.",
        ),
        Missing::Internal => {
            statics::html_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        }
    }
}

fn missing_json(m: Missing) -> ApiError {
    match m {
        Missing::NotFound => ApiError::not_found(),
        Missing::Gone => ApiError::gone(),
        Missing::Internal => ApiError::internal(),
    }
}

pub fn missing_json_public(m: Missing) -> Response {
    missing_json(m).into_response()
}

pub async fn resolve_share(app: &App, token: &str) -> Result<ShareRow, Missing> {
    let t = token.to_string();
    let row = app
        .db
        .call(move |c| Db::get_share(c, &t))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "share lookup failed");
            Missing::Internal
        })?;
    match row {
        None => Err(Missing::NotFound),
        Some(share) if share.is_active(db::now()) => Ok(share),
        Some(_) => Err(Missing::Gone),
    }
}

/// `(project, slug)` behind either a share token or a signed view cap.
pub async fn resolve_cap(app: &App, cap: &str) -> Result<(String, String), Missing> {
    if caps::is_view_cap(cap) {
        match app.caps.verify(cap) {
            Some((project, slug, expires)) => {
                if expires <= db::now() {
                    Err(Missing::Gone)
                } else {
                    Ok((project, slug))
                }
            }
            None => Err(Missing::NotFound),
        }
    } else {
        let share = resolve_share(app, cap).await?;
        Ok((share.project, share.slug))
    }
}

async fn load_report(app: &App, project: &str, slug: &str) -> Result<ReportRow, Missing> {
    let (p, s) = (project.to_string(), slug.to_string());
    let row = app
        .db
        .call(move |c| Db::get_report(c, &p, &s))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "report lookup failed");
            Missing::Internal
        })?;
    row.ok_or(Missing::NotFound)
}

// ---------------------------------------------------------------- /api/s/:token

pub async fn share_meta(State(app): State<App>, Path(token): Path<String>) -> Response {
    let share = match resolve_share(&app, &token).await {
        Ok(s) => s,
        Err(m) => return missing_json(m).into_response(),
    };
    let report = match load_report(&app, &share.project, &share.slug).await {
        Ok(r) => r,
        Err(m) => return missing_json(m).into_response(),
    };
    axum::Json(json!({
        "project": report.project,
        "slug": report.slug,
        "title": report.title,
        "description": report.description,
        "rev": report.rev,
        "updated_at": report.updated_at,
        "expires_at": share.expires_at,
    }))
    .into_response()
}

pub async fn share_body(State(app): State<App>, Path(token): Path<String>) -> Response {
    let share = match resolve_share(&app, &token).await {
        Ok(s) => s,
        Err(m) => return missing_json(m).into_response(),
    };
    match load_report(&app, &share.project, &share.slug).await {
        Ok(report) => api::body_response(&report),
        Err(m) => missing_json(m).into_response(),
    }
}

// ---------------------------------------------------------------- /api/c/:cap

pub async fn cap_redirect(Path(cap): Path<String>) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::PERMANENT_REDIRECT;
    match HeaderValue::from_str(&format!("/api/c/{cap}/")) {
        Ok(location) => {
            resp.headers_mut().insert(header::LOCATION, location);
            resp
        }
        Err(_) => statics::html_error(StatusCode::BAD_REQUEST, "invalid capability"),
    }
}

pub async fn cap_document(State(app): State<App>, Path(cap): Path<String>) -> Response {
    let (project, slug) = match resolve_cap(&app, &cap).await {
        Ok(target) => target,
        Err(m) => return missing_html(m),
    };
    let report = match load_report(&app, &project, &slug).await {
        Ok(r) => r,
        Err(m) => return missing_html(m),
    };
    let fragment = source::fragment_html(source::body_of(&report.source));
    let html = doc::document(&report, &fragment, &cap, &app.version);

    let mut resp = (StatusCode::OK, html).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    match HeaderValue::from_str(&doc::csp(&app.cfg.public_url, &cap)) {
        Ok(value) => {
            resp.headers_mut()
                .insert(header::CONTENT_SECURITY_POLICY, value);
        }
        Err(_) => return statics::html_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid CSP"),
    }
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

pub async fn cap_asset(
    State(app): State<App>,
    Path((cap, name)): Path<(String, String)>,
) -> Response {
    let (project, slug) = match resolve_cap(&app, &cap).await {
        Ok(target) => target,
        Err(m) => return missing_html(m),
    };
    let (p, s, n) = (project.clone(), slug.clone(), name.clone());
    let asset = app.db.call(move |c| Db::get_asset(c, &p, &s, &n)).await;
    let Ok(Some(asset)) = asset else {
        return statics::html_error(StatusCode::NOT_FOUND, "Asset not found.");
    };
    let path = app.cfg.assets_dir().join(&asset.sha256);
    let is_svg = asset.content_type == "image/svg+xml";
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            let headers = resp.headers_mut();
            let content_type = HeaderValue::from_str(&asset.content_type)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            headers.insert(header::CONTENT_TYPE, content_type);
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=300"),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            if is_svg {
                headers.insert(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("sandbox"),
                );
            }
            resp
        }
        Err(_) => statics::html_error(StatusCode::NOT_FOUND, "Asset not found."),
    }
}

// ---------------------------------------------------------------- preflight

pub async fn preflight(headers: HeaderMap) -> Response {
    statics::cors_preflight(&headers)
}
