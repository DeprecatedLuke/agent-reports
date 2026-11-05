use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::Duration;

use crate::{
    App, caps,
    config::{ensure_ident, valid_asset_name},
    db::{self, AssetJson, AssetRow, Db, PutResult, ReportRow, ReportSummary, ShareRow},
    doc,
    error::ApiError,
    source,
};

const MAX_SOURCE: usize = 5 * 1024 * 1024;
const MAX_ASSET: usize = 25 * 1024 * 1024;

const ALLOWED_ASSET_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/svg+xml",
    "text/csv",
    "application/json",
    "text/plain",
];

fn validation_problem(message: &str) -> ApiError {
    ApiError::validation(json!([{ "message": message }]))
}

fn ident_or_422(p: &str, s: &str) -> Result<(), ApiError> {
    ensure_ident(p, s).map_err(|msg| validation_problem(&msg))
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn summary(row: &ReportRow) -> Value {
    serde_json::to_value(ReportSummary::from(row)).unwrap_or(Value::Null)
}

fn share_json(app: &App, share: &ShareRow) -> Value {
    json!({
        "token": share.token,
        "url": format!("{}/s/{}", app.cfg.public_url, share.token),
        "created_at": share.created_at,
        "expires_at": share.expires_at,
    })
}

async fn read_limited(body: Body, limit: usize) -> Result<Vec<u8>, ApiError> {
    let bytes = axum::body::to_bytes(body, limit + 1)
        .await
        .map_err(|_| ApiError::payload_too_large())?;
    if bytes.len() > limit {
        return Err(ApiError::payload_too_large());
    }
    Ok(bytes.to_vec())
}

fn if_match_rev(headers: &HeaderMap) -> Result<Option<i64>, ApiError> {
    let Some(raw) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .map_err(|_| ApiError::bad_request("invalid If-Match header"))?
        .trim()
        .to_string();
    if raw == "*" {
        return Ok(None);
    }
    let raw = raw.strip_prefix("W/").unwrap_or(&raw).trim();
    let unquoted = raw.trim_matches('"');
    unquoted
        .parse::<i64>()
        .map(Some)
        .map_err(|_| ApiError::bad_request("invalid If-Match header (expected \"<rev>\")"))
}

fn if_none_match_star(headers: &HeaderMap) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|part| part.trim() == "*"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- basics

pub async fn me() -> Response {
    Json(json!({"ok": true})).into_response()
}

pub async fn healthz() -> Response {
    (StatusCode::OK, "ok").into_response()
}

/// Every unmatched path, any method.
pub async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
}

pub async fn projects(State(app): State<App>) -> Result<Response, ApiError> {
    let rows = app.db.call(|c| Db::list_projects(c)).await?;
    Ok(Json(json!({ "projects": rows })).into_response())
}

#[derive(Debug, Deserialize)]
pub struct ReportsQuery {
    project: Option<String>,
}

pub async fn list_reports(
    State(app): State<App>,
    Query(q): Query<ReportsQuery>,
) -> Result<Response, ApiError> {
    if let Some(p) = &q.project
        && !crate::config::valid_ident(p)
    {
        return Err(validation_problem(
            "project must match ^[a-z0-9][a-z0-9_-]{0,63}$",
        ));
    }
    let project = q.project.clone();
    let rows = app
        .db
        .call(move |c| Db::list_reports(c, project.as_deref()))
        .await?;
    Ok(Json(json!({ "reports": rows })).into_response())
}

pub async fn get_report(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let row = fetch_report(&app, &p, &s).await?;
    let shares = app
        .db
        .call({
            let (p, s) = (p.clone(), s.clone());
            move |c| Db::shares_for(c, &p, &s)
        })
        .await?;
    let now = db::now();
    let active: Vec<Value> = shares
        .iter()
        .filter(|share| share.is_active(now))
        .map(|share| share_json(&app, share))
        .collect();
    let mut summary = summary(&row);
    if let Value::Object(map) = &mut summary {
        map.insert("shares".into(), Value::Array(active));
    }
    Ok(Json(summary).into_response())
}

async fn fetch_report(app: &App, p: &str, s: &str) -> Result<ReportRow, ApiError> {
    let (pp, ss) = (p.to_string(), s.to_string());
    let row = app.db.call(move |c| Db::get_report(c, &pp, &ss)).await?;
    row.ok_or_else(ApiError::not_found)
}

// ---------------------------------------------------------------- source

pub async fn get_source(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let row = fetch_report(&app, &p, &s).await?;
    Ok(source_response(row.rev, row.source))
}

fn source_response(rev: i64, body: String) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{rev}\"")).expect("etag is ASCII"),
    );
    headers.insert(
        "x-agent-reports-rev",
        HeaderValue::from_str(&rev.to_string()).expect("rev is ASCII"),
    );
    resp
}

pub async fn put_source(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let if_match = if_match_rev(&headers)?;
    let if_none_match = if_none_match_star(&headers);

    let bytes = read_limited(body, MAX_SOURCE).await?;
    let source = String::from_utf8(bytes)
        .map_err(|_| ApiError::bad_request("source must be valid UTF-8"))?;

    let parsed = match source::parse_and_validate(&source, &app.icons) {
        Ok(parsed) => parsed,
        Err(problems) => {
            return Err(ApiError::validation(
                serde_json::to_value(&problems).unwrap_or(Value::Array(Vec::new())),
            ));
        }
    };

    let markdown = app
        .md
        .convert(&source::fragment_html(&parsed.body))
        .unwrap_or_default();
    let at = db::now();
    let args = (
        p.clone(),
        s.clone(),
        parsed.title.clone(),
        parsed.description.clone(),
        source.clone(),
        markdown,
    );
    let result = app
        .db
        .call(move |c| {
            Db::put_report(
                c,
                &args.0,
                &args.1,
                &args.2,
                &args.3,
                &args.4,
                &args.5,
                if_match,
                if_none_match,
                at,
            )
        })
        .await?;

    match result {
        PutResult::Created(row) => {
            let _ = app
                .events
                .send(crate::events::Event::ReportUpserted(ReportSummary::from(
                    &row,
                )));
            Ok(json_response(
                StatusCode::OK,
                json!({
                    "report": summary(&row),
                    "created": true,
                    "url": format!("{}/r/{}/{}", app.cfg.public_url, row.project, row.slug),
                }),
            ))
        }
        PutResult::Updated(row) => {
            let _ = app
                .events
                .send(crate::events::Event::ReportUpserted(ReportSummary::from(
                    &row,
                )));
            Ok(json_response(
                StatusCode::OK,
                json!({
                    "report": summary(&row),
                    "created": false,
                    "url": format!("{}/r/{}/{}", app.cfg.public_url, row.project, row.slug),
                }),
            ))
        }
        PutResult::Unchanged(row) => Ok(json_response(
            StatusCode::OK,
            json!({
                "report": summary(&row),
                "created": false,
                "url": format!("{}/r/{}/{}", app.cfg.public_url, row.project, row.slug),
            }),
        )),
        PutResult::RevMismatch { current_rev } => Ok(json_response(
            StatusCode::PRECONDITION_FAILED,
            json!({"error": "rev_mismatch", "current_rev": current_rev}),
        )),
        PutResult::Exists { current_rev } => Ok(json_response(
            StatusCode::PRECONDITION_FAILED,
            json!({"error": "exists", "current_rev": current_rev}),
        )),
    }
}

pub async fn delete_report(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    fetch_report(&app, &p, &s).await?;
    let (pp, ss) = (p.clone(), s.clone());
    let orphans = app.db.call(move |c| Db::delete_report(c, &pp, &ss)).await?;
    for sha in orphans {
        let path = app.cfg.assets_dir().join(sha);
        let _ = tokio::fs::remove_file(path).await;
    }
    let _ = app.events.send(crate::events::Event::ReportDeleted {
        project: p,
        slug: s,
    });
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn get_text(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let row = fetch_report(&app, &p, &s).await?;
    let mut resp = (StatusCode::OK, row.text).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    resp.headers_mut().insert(
        "x-agent-reports-rev",
        HeaderValue::from_str(&row.rev.to_string()).expect("rev is ASCII"),
    );
    Ok(resp)
}

pub async fn get_body(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let row = fetch_report(&app, &p, &s).await?;
    Ok(body_response(&row))
}

pub fn body_response(row: &ReportRow) -> Response {
    let fragment = source::fragment_html(source::body_of(&row.source));
    let html = doc::body_html_with_fragment(row, &fragment);
    let mut resp = (StatusCode::OK, html).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        "x-agent-reports-rev",
        HeaderValue::from_str(&row.rev.to_string()).expect("rev is ASCII"),
    );
    // Report HTML must never run on the app origin, even if opened directly.
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    resp
}

// ---------------------------------------------------------------- assets

pub async fn list_assets(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let (pp, ss) = (p.clone(), s.clone());
    let rows = app.db.call(move |c| Db::list_assets(c, &pp, &ss)).await?;
    let assets: Vec<AssetJson> = rows.iter().map(AssetJson::from).collect();
    Ok(Json(json!({ "assets": assets })).into_response())
}

pub async fn put_asset(
    State(app): State<App>,
    Path((p, s, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    if !valid_asset_name(&name) {
        return Err(ApiError::bad_request(
            "asset name must match ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$",
        ));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                json!({"error": "content_type_required"}),
            )
        })?;
    if !ALLOWED_ASSET_TYPES.contains(&content_type.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({"error": "unsupported_media_type", "allowed": ALLOWED_ASSET_TYPES}),
        ));
    }

    let bytes = read_limited(body, MAX_ASSET).await?;
    let sha = hex::encode(Sha256::digest(&bytes));
    let asset = AssetRow {
        project: p.clone(),
        slug: s.clone(),
        name: name.clone(),
        sha256: sha.clone(),
        content_type: content_type.clone(),
        size: bytes.len() as i64,
        created_at: db::fmt_time(db::now()),
    };

    store_blob(&app, &sha, &bytes).await?;

    let stored = asset.clone();
    app.db.call(move |c| Db::upsert_asset(c, &stored)).await?;

    if let Ok(row) = fetch_report(&app, &p, &s).await {
        let _ = app
            .events
            .send(crate::events::Event::ReportUpserted(ReportSummary::from(
                &row,
            )));
    }

    Ok(Json(json!({
        "name": asset.name,
        "content_type": asset.content_type,
        "size": asset.size,
        "sha256": asset.sha256,
        "src": format!("assets/{}", asset.name),
    }))
    .into_response())
}

async fn store_blob(app: &App, sha: &str, bytes: &[u8]) -> Result<(), ApiError> {
    let dir = app.cfg.assets_dir();
    tokio::fs::create_dir_all(&dir).await.map_err(|err| {
        tracing::error!(error = %err, "creating assets dir");
        ApiError::internal()
    })?;
    let path = dir.join(sha);
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(());
    }
    let tmp = dir.join(format!(".tmp-{sha}-{}", std::process::id()));
    tokio::fs::write(&tmp, bytes).await.map_err(|err| {
        tracing::error!(error = %err, "writing asset blob");
        ApiError::internal()
    })?;
    tokio::fs::rename(&tmp, &path).await.map_err(|err| {
        tracing::error!(error = %err, "renaming asset blob");
        ApiError::internal()
    })?;
    Ok(())
}

pub async fn get_asset(
    State(app): State<App>,
    Path((p, s, name)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let (pp, ss, nn) = (p.clone(), s.clone(), name.clone());
    let asset = app
        .db
        .call(move |c| Db::get_asset(c, &pp, &ss, &nn))
        .await?
        .ok_or_else(ApiError::not_found)?;
    let path = app.cfg.assets_dir().join(&asset.sha256);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let mut resp = (StatusCode::OK, bytes).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&asset.content_type).map_err(|_| ApiError::internal())?,
    );
    // Uploaded report assets are served from the app origin; never let them run there.
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    Ok(resp)
}

pub async fn delete_asset(
    State(app): State<App>,
    Path((p, s, name)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let (pp, ss, nn) = (p.clone(), s.clone(), name.clone());
    let deleted = app
        .db
        .call(move |c| Db::delete_asset(c, &pp, &ss, &nn))
        .await?;
    match deleted {
        None => Err(ApiError::not_found()),
        Some(orphan) => {
            if let Some(sha) = orphan {
                let _ = tokio::fs::remove_file(app.cfg.assets_dir().join(sha)).await;
            }
            Ok(StatusCode::NO_CONTENT.into_response())
        }
    }
}

// ---------------------------------------------------------------- shares

pub async fn create_share(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    fetch_report(&app, &p, &s).await?;
    let at = db::now();
    let expires = at + Duration::seconds(caps::SHARE_TTL_SECS);
    let share = ShareRow {
        token: caps::new_share_token(),
        project: p.clone(),
        slug: s.clone(),
        created_at: db::fmt_time(at),
        expires_at: db::fmt_time(expires),
        revoked_at: None,
    };
    let stored = share.clone();
    app.db.call(move |c| Db::create_share(c, &stored)).await?;
    let _ = app.events.send(crate::events::Event::ShareChanged {
        project: p,
        slug: s,
    });
    Ok(json_response(StatusCode::CREATED, share_json(&app, &share)))
}

pub async fn list_shares(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    let (pp, ss) = (p.clone(), s.clone());
    let rows = app.db.call(move |c| Db::shares_for(c, &pp, &ss)).await?;
    let now = db::now();
    let active: Vec<Value> = rows
        .iter()
        .filter(|share| share.is_active(now))
        .map(|share| share_json(&app, share))
        .collect();
    Ok(Json(json!({ "shares": active })).into_response())
}

pub async fn revoke_share(
    State(app): State<App>,
    Path(token): Path<String>,
) -> Result<Response, ApiError> {
    let lookup = token.clone();
    let existing = app
        .db
        .call(move |c| Db::get_share(c, &lookup))
        .await?
        .ok_or_else(ApiError::not_found)?;
    let to_revoke = token.clone();
    let revoked = app
        .db
        .call(move |c| Db::revoke_share(c, &to_revoke, db::now()))
        .await?;
    if revoked.is_some() {
        let _ = app.events.send(crate::events::Event::ShareChanged {
            project: existing.project,
            slug: existing.slug,
        });
        let _ = app.events.send(crate::events::Event::ShareRevoked {
            token: token.clone(),
        });
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn view_cap(
    State(app): State<App>,
    Path((p, s)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    ident_or_422(&p, &s)?;
    fetch_report(&app, &p, &s).await?;
    let expires = db::now() + Duration::seconds(caps::VIEW_CAP_TTL_SECS);
    let cap = app.caps.issue(&p, &s, expires);
    Ok(Json(json!({
        "cap": cap,
        "expires_at": db::fmt_time(expires),
    }))
    .into_response())
}

// ---------------------------------------------------------------- helpers
