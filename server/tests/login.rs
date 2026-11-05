mod common;

use common::{PASSWORD, SESSION, SESSION_COOKIE, Server};
use reqwest::{Method, header};

const CHALLENGE: &str = r#"Basic realm="agent-reports", charset="UTF-8""#;

fn assert_challenge(resp: &common::Resp, case: &str) {
    assert_eq!(resp.status, 401, "{case}");
    assert_eq!(resp.header("www-authenticate"), Some(CHALLENGE), "{case}");
    assert_eq!(resp.header("set-cookie"), None, "{case}");
}

#[tokio::test]
async fn login_rejects_bad_credentials_with_basic_challenge() {
    let srv = Server::start();
    let cases: [(&str, Option<&str>); 5] = [
        ("missing header", None),
        ("wrong password", Some("Basic dGVzdGVyOndyb25n")), // tester:wrong
        ("wrong user", Some("Basic b3RoZXI6dGVzdGVyLXBhc3N3b3Jk")), // other:tester-password
        ("malformed base64", Some("Basic !!!not-base64")),
        (
            "non-Basic scheme",
            Some("Bearer dGVzdGVyOnRlc3Rlci1wYXNzd29yZA=="),
        ),
    ];
    for (case, auth) in cases {
        let mut req = srv.anon(Method::GET, "/api/login?next=/x");
        if let Some(auth) = auth {
            req = req.header(header::AUTHORIZATION, auth);
        }
        assert_challenge(&srv.send(req).await, case);
    }
}

#[tokio::test]
async fn valid_login_sets_session_cookie_that_unlocks_the_api() {
    let srv = Server::start();
    let resp = srv
        .send(
            srv.anon(Method::GET, "/api/login?next=/ok?x=1")
                .basic_auth("tester", Some(PASSWORD)),
        )
        .await;
    assert_eq!(resp.status, 303);
    assert_eq!(resp.header("location"), Some("/ok?x=1"));
    let set_cookie = resp.header("set-cookie").expect("Set-Cookie");
    assert_eq!(
        set_cookie,
        format!(
            "{SESSION_COOKIE}={SESSION}; Path=/; Max-Age=34560000; Secure; HttpOnly; SameSite=Lax"
        )
    );

    let pair = set_cookie.split(';').next().unwrap().to_string();
    let me = srv
        .send(
            srv.anon(Method::GET, "/api/me")
                .header(header::COOKIE, pair),
        )
        .await;
    assert_eq!(me.status, 200);
}

#[tokio::test]
async fn existing_session_redirects_without_reissuing_cookie() {
    let srv = Server::start();
    let resp = srv
        .send(srv.owner(Method::GET, "/api/login?next=/back"))
        .await;
    assert_eq!(resp.status, 303);
    assert_eq!(resp.header("location"), Some("/back"));
    assert_eq!(resp.header("set-cookie"), None);
}

#[tokio::test]
async fn next_is_restricted_to_local_paths() {
    let srv = Server::start();
    let cases = [
        (
            "local path with query kept",
            "?next=%2Fok%3Fx%3D1",
            "/ok?x=1",
        ),
        ("protocol-relative", "?next=//evil.test/x", "/"),
        ("backslash", "?next=/%5Cevil", "/"),
        ("absolute URL", "?next=https://evil.test/", "/"),
        ("empty", "?next=", "/"),
        ("missing", "", "/"),
        ("CRLF", "?next=/a%0d%0aX:1", "/"),
    ];
    for (case, query, want) in cases {
        let resp = srv
            .send(srv.owner(Method::GET, &format!("/api/login{query}")))
            .await;
        assert_eq!(resp.status, 303, "{case}");
        assert_eq!(resp.header("location"), Some(want), "{case}");
    }
}

#[tokio::test]
async fn login_only_accepts_get() {
    let srv = Server::start();
    let resp = srv
        .send(
            srv.anon(Method::POST, "/api/login")
                .basic_auth("tester", Some(PASSWORD)),
        )
        .await;
    assert_eq!(resp.status, 405);
    assert_eq!(resp.header("set-cookie"), None);
}

#[tokio::test]
async fn login_without_htpasswd_never_succeeds() {
    let srv = Server::start_without_htpasswd();
    let resp = srv
        .send(
            srv.anon(Method::GET, "/api/login?next=/")
                .basic_auth("tester", Some(PASSWORD)),
        )
        .await;
    assert_challenge(&resp, "no htpasswd configured");
}
