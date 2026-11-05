use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};

/// `OPTIONS` response for the CORS-enabled public GET endpoints. Sandboxed
/// iframe module fetches are simple requests, but tooling that injects
/// `X-Agent-Reports-Owner` into a tab triggers a preflight; answer it.
pub fn cors_preflight(headers: &HeaderMap) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    let h = resp.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        headers
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("*")),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    resp
}

pub fn html_error(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{code} {reason}</title></head>\
<body style=\"font-family: system-ui, sans-serif; padding: 2rem; color: #1b1f24\"><h1>{code} {reason}</h1><p>{message}</p></body></html>",
        code = status.as_u16(),
        reason = status.canonical_reason().unwrap_or("Error"),
        message = crate::doc::escape(message),
    );
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}
