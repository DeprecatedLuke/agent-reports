use axum::{
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::App;

/// Session cookie carrying [`crate::config::Config::session_token`]. The `__Host-` prefix
/// pins it to this origin, `/`, and `Secure`.
pub const SESSION_COOKIE: &str = "__Host-agent-reports";

const SESSION_MAX_AGE: u32 = 34_560_000;

/// Paths reachable without a session cookie. Assets, share pages and view caps are public;
/// `/api/login` mints the cookie and `/api/guard` checks it itself.
pub fn is_public(path: &str) -> bool {
    fn under(path: &str, prefix: &str) -> bool {
        path.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
    }
    path == "/api/healthz"
        || path == "/api/login"
        || path == "/api/guard"
        || under(path, "/api/s/")
        || under(path, "/api/c/")
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

/// Value of the session cookie across every `Cookie` header, `name=value` pairs split on `;`.
fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| name.trim() == SESSION_COOKIE)
        .map(|(_, value)| value.trim())
}

/// Constant-time check of a presented cookie value. Compares SHA-256 digests so the
/// comparison is fixed-width regardless of length.
fn session_matches(expected: &str, presented: &str) -> bool {
    let given = Sha256::digest(presented.as_bytes());
    let wanted = Sha256::digest(expected.as_bytes());
    bool::from(given.ct_eq(&wanted))
}

fn has_session(app: &App, headers: &HeaderMap) -> bool {
    session_cookie(headers).is_some_and(|v| session_matches(&app.cfg.session_token, v))
}

/// Gate for every route that is not public: the request needs the session cookie.
pub async fn gate(State(app): State<App>, req: Request, next: Next) -> Response {
    if is_public(req.uri().path()) || has_session(&app, req.headers()) {
        next.run(req).await
    } else {
        unauthorized()
    }
}

#[derive(Debug, Default, Deserialize)]
struct LoginQuery {
    #[serde(default)]
    next: Option<String>,
}

/// A same-site path, or `/`: absolute, protocol-relative and control-character values are
/// dropped so the redirect cannot leave this origin.
fn safe_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.chars().any(char::is_control);
    if ok {
        next.to_string()
    } else {
        "/".to_string()
    }
}

/// `Basic` credentials from the `Authorization` header, as raw bytes so a password that is
/// not UTF-8 still verifies.
fn basic_credentials(headers: &HeaderMap) -> Option<(Vec<u8>, Vec<u8>)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let raw = STANDARD.decode(encoded.trim()).ok()?;
    let colon = raw.iter().position(|b| *b == b':')?;
    Some((raw[..colon].to_vec(), raw[colon + 1..].to_vec()))
}

fn cookie_header(token: &str) -> HeaderValue {
    let value = format!(
        "{SESSION_COOKIE}={token}; Path=/; Max-Age={SESSION_MAX_AGE}; Secure; HttpOnly; SameSite=Lax"
    );
    HeaderValue::from_str(&value).expect("session token is validated as a cookie value")
}

fn see_other(next: &str, cookie: Option<HeaderValue>) -> Response {
    let location = HeaderValue::from_str(next).unwrap_or_else(|_| HeaderValue::from_static("/"));
    let mut resp = (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
    if let Some(cookie) = cookie {
        resp.headers_mut().append(header::SET_COOKIE, cookie);
    }
    resp
}

fn challenge() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"agent-reports\", charset=\"UTF-8\""),
            ),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
        ],
        "<!doctype html><title>Sign in required</title><h1>Sign in required</h1>",
    )
        .into_response()
}

/// `GET /api/login?next=<path>`: the only route that accepts `Basic` credentials. A valid
/// session redirects straight through; otherwise the htpasswd pair mints one.
pub async fn login(State(app): State<App>, headers: HeaderMap, uri: Uri) -> Response {
    let next = Query::<LoginQuery>::try_from_uri(&uri)
        .ok()
        .and_then(|query| query.0.next)
        .unwrap_or_default();
    let next = safe_next(&next);

    let token = app.cfg.session_token.clone();
    if session_cookie(&headers).is_some_and(|v| session_matches(&token, v)) {
        return see_other(&next, None);
    }

    let credentials = basic_credentials(&headers);
    let Some(entry) = app.cfg.htpasswd.as_ref() else {
        return challenge();
    };
    let Some((user, password)) = credentials else {
        return challenge();
    };
    if !session_matches(&entry.user, &String::from_utf8_lossy(&user)) {
        return challenge();
    }

    let hash = entry.hash.clone();
    let verified = tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash))
        .await
        .unwrap_or(Ok(false))
        .unwrap_or(false);
    if verified {
        see_other(&next, Some(cookie_header(&token)))
    } else {
        challenge()
    }
}

/// Header Traefik's `forwardAuth` sets to the original request path and query.
const FORWARDED_URI: &str = "x-forwarded-uri";

/// `GET /api/guard`: Traefik `forwardAuth` target for app pages. A valid session gets `204`;
/// anything else gets a `302` to `/api/login?next=<original path>`, which Traefik hands
/// to the browser unchanged.
pub async fn guard(State(app): State<App>, headers: HeaderMap) -> Response {
    if has_session(&app, &headers) {
        return StatusCode::NO_CONTENT.into_response();
    }
    let next = headers
        .get(FORWARDED_URI)
        .and_then(|value| value.to_str().ok())
        .map_or_else(|| "/".to_string(), safe_next);
    let next: String = form_urlencoded::byte_serialize(next.as_bytes()).collect();
    let location = HeaderValue::from_str(&format!("/api/login?next={next}"))
        .expect("percent-encoded location is a valid header value");
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, location),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
    )
        .into_response()
}
