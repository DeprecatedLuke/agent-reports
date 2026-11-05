mod common;

use common::{SESSION, SESSION_COOKIE, Server, doc};
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn owner_routes_reject_missing_or_wrong_session_cookie() {
    let srv = Server::start();
    let cookie = |value: &str| format!("{SESSION_COOKIE}={value}");
    let wrong = [
        // No `=value` pair, and an empty value.
        SESSION_COOKIE.to_string(),
        cookie(""),
        cookie("wrong"),
        // A strict prefix / extension of the token must not pass.
        cookie(&SESSION[..SESSION.len() - 1]),
        cookie(&format!("{SESSION}x")),
        // The right value under a cookie name the server does not read.
        format!("agent-reports-session={SESSION}"),
        format!("a=b; token={SESSION}"),
    ];
    for path in ["/api/me", "/api/reports", "/api/projects", "/r/p/s", "/"] {
        let resp = srv.send(srv.anon(Method::GET, path)).await;
        assert_eq!(resp.status, 401, "{path} with no cookie");
        for value in &wrong {
            let resp = srv
                .send(srv.anon(Method::GET, path).header("cookie", value.as_str()))
                .await;
            assert_eq!(resp.status, 401, "{path} with cookie {value:?}");
            assert_eq!(resp.json(), json!({"error": "unauthorized"}), "{path}");
            // No challenge header: a background fetch must never open a browser prompt.
            assert_eq!(resp.header("www-authenticate"), None, "{path}");
            assert_eq!(
                resp.header("x-content-type-options"),
                Some("nosniff"),
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn unauthenticated_writes_are_rejected_and_have_no_effect() {
    let srv = Server::start();
    let resp = srv
        .send(
            srv.anon(Method::PUT, "/api/reports/p/s/source")
                .body(doc("T", "<p>x</p>")),
        )
        .await;
    assert_eq!(resp.status, 401);
    assert_eq!(srv.get("/api/reports/p/s").await.status, 404);
}

#[tokio::test]
async fn session_cookie_unlocks_api_and_unknown_paths_are_json_404() {
    let srv = Server::start();
    let me = srv.get("/api/me").await;
    assert_eq!(me.status, 200);
    assert_eq!(me.json(), json!({"ok": true}));

    // The session cookie is found among others in the same header.
    let among = format!("a=b; {SESSION_COOKIE}={SESSION}; c=d");
    let resp = srv
        .send(srv.anon(Method::GET, "/api/me").header("cookie", among))
        .await;
    assert_eq!(resp.status, 200);

    // Basic credentials are only accepted by `/api/login`, never by the gate.
    let basic = srv
        .send(
            srv.anon(Method::GET, "/api/me")
                .basic_auth("tester", Some(common::PASSWORD)),
        )
        .await;
    assert_eq!(basic.status, 401);
    assert_eq!(basic.header("www-authenticate"), None);

    // The server serves no HTML: everything outside `/api` is a JSON 404.
    for path in ["/", "/r/p/s", "/s/x/meta", "/c/x/", "/healthz"] {
        let resp = srv.get(path).await;
        assert_eq!(resp.status, 404, "{path}");
        assert_eq!(resp.json(), json!({"error": "not found"}), "{path}");
        assert_eq!(
            resp.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn public_paths_work_without_session_cookie() {
    let srv = Server::start();

    let health = srv.get_anon("/api/healthz").await;
    assert_eq!((health.status, health.text().as_str()), (200, "ok"));

    // Unknown share/cap: the public handler answers (404), not the gate (401).
    for path in ["/api/s/nope/meta", "/api/c/nope/"] {
        assert_eq!(srv.get_anon(path).await.status, 404, "{path}");
    }

    srv.upsert("p", "s", "Public", "<p>public-body-marker</p>")
        .await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let meta = srv.get_anon(&format!("/api/s/{token}/meta")).await;
    assert_eq!(meta.status, 200);
    assert_eq!(meta.json()["title"], "Public");

    let body = srv.get_anon(&format!("/api/s/{token}/body")).await;
    assert_eq!(body.status, 200);
    assert!(body.text().contains("public-body-marker"));
    assert_eq!(body.header("content-security-policy"), Some(SANDBOX_CSP));
    assert_eq!(body.header("x-content-type-options"), Some("nosniff"));

    let document = srv.get_anon(&format!("/api/c/{token}/")).await;
    assert_eq!(document.status, 200);
    assert!(document.text().contains("public-body-marker"));
}

const SANDBOX_CSP: &str = "sandbox; default-src 'none'";

/// Report HTML and uploaded assets on authenticated routes are untrusted content served from
/// the API origin: they must be sandboxed so scripts in them cannot act with the session.
#[tokio::test]
async fn owner_body_and_assets_are_sandboxed() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>owner-body-marker</p>").await;

    let body = srv.get("/api/reports/p/s/body").await;
    assert_eq!(body.status, 200);
    assert!(body.text().contains("owner-body-marker"));
    assert_eq!(body.header("content-security-policy"), Some(SANDBOX_CSP));
    assert_eq!(body.header("x-content-type-options"), Some("nosniff"));

    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    let put = srv
        .put_asset("p", "s", "evil.svg", Some("image/svg+xml"), svg.to_vec())
        .await;
    assert!(put.status < 300, "asset upload failed: {}", put.text());
    let asset = srv.get("/api/reports/p/s/assets/evil.svg").await;
    assert_eq!(asset.status, 200);
    assert_eq!(asset.body, svg.to_vec());
    assert_eq!(asset.header("content-security-policy"), Some(SANDBOX_CSP));
}

#[tokio::test]
async fn prefixed_public_paths_and_removed_prefixes() {
    let srv = Server::start();
    srv.upsert("p", "s", "Public", "<p>x</p>").await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // `/api/c/{cap}` redirects into the trailing-slash document route.
    let redirect = srv.get_anon(&format!("/api/c/{token}")).await;
    assert_eq!(redirect.status, 308);
    assert_eq!(
        redirect.header("location"),
        Some(format!("/api/c/{token}/").as_str())
    );

    // The old unprefixed public routes are gone; with the session cookie they reach
    // the JSON 404 instead of a handler.
    for path in [
        format!("/s/{token}"),
        format!("/s/{token}/meta"),
        format!("/s/{token}/body"),
        format!("/c/{token}/"),
        "/_render/runtime.js".to_string(),
        "/assets/index.js".to_string(),
        "/favicon.svg".to_string(),
    ] {
        let resp = srv.get(&path).await;
        assert_eq!(resp.status, 404, "{path}");
        assert_eq!(resp.json(), json!({"error": "not found"}), "{path}");
    }
}
