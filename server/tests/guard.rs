//! `GET /api/guard`: the Traefik `forwardAuth` target that decides whether an app page
//! may be served at all. Traefik sets `X-Forwarded-Uri` to the original path and query.

mod common;

use common::{SESSION_COOKIE, Server};
use reqwest::{Method, header};

const FORWARDED_URI: &str = "x-forwarded-uri";

#[tokio::test]
async fn guard_answers_on_the_session_cookie() {
    let srv = Server::start();

    struct Case {
        name: &'static str,
        cookie: Option<String>,
        forwarded: Option<&'static str>,
        status: u16,
        location: Option<&'static str>,
    }

    let cases = [
        Case {
            name: "valid cookie",
            cookie: Some(srv.cookie()),
            forwarded: None,
            status: 204,
            location: None,
        },
        Case {
            name: "no cookie",
            cookie: None,
            forwarded: Some("/r/p/s?r=a&r=b"),
            status: 302,
            location: Some("/api/login?next=%2Fr%2Fp%2Fs%3Fr%3Da%26r%3Db"),
        },
        Case {
            name: "missing forwarded uri",
            cookie: None,
            forwarded: None,
            status: 302,
            location: Some("/api/login?next=%2F"),
        },
        Case {
            name: "protocol-relative forwarded uri",
            cookie: None,
            forwarded: Some("//evil.test/x"),
            status: 302,
            location: Some("/api/login?next=%2F"),
        },
        Case {
            name: "wrong cookie value",
            cookie: Some(format!("{SESSION_COOKIE}=not-the-token")),
            forwarded: Some("/r/p/s"),
            status: 302,
            location: Some("/api/login?next=%2Fr%2Fp%2Fs"),
        },
    ];

    for case in cases {
        let case = &case;
        let mut req = srv.anon(Method::GET, "/api/guard");
        if let Some(cookie) = &case.cookie {
            req = req.header(header::COOKIE, cookie);
        }
        if let Some(forwarded) = case.forwarded {
            req = req.header(FORWARDED_URI, forwarded);
        }
        let resp = srv.send(req).await;
        assert_eq!(resp.status, case.status, "{}", case.name);
        assert_eq!(resp.header("location"), case.location, "{}", case.name);
        assert!(resp.body.is_empty(), "{}: no body expected", case.name);
        let cache = resp.header("cache-control");
        if case.status == 302 {
            assert_eq!(cache, Some("no-store"), "{}", case.name);
        } else {
            assert_eq!(cache, None, "{}", case.name);
        }
    }
}

/// The `next` the guard builds is the one `/api/login` reads back: signing in from the
/// browser's redirect lands on the page the browser asked for, query included.
#[tokio::test]
async fn guard_redirect_round_trips_through_login() {
    let srv = Server::start();
    let blocked = srv
        .send(
            srv.anon(Method::GET, "/api/guard")
                .header(FORWARDED_URI, "/r/p/s?r=a&r=b"),
        )
        .await;
    assert_eq!(blocked.status, 302);
    let location = blocked.header("location").expect("Location").to_string();

    let followed = srv.send(srv.owner(Method::GET, &location)).await;
    assert_eq!(followed.status, 303);
    assert_eq!(followed.header("location"), Some("/r/p/s?r=a&r=b"));
}

#[tokio::test]
async fn guard_only_accepts_get() {
    let srv = Server::start();
    let resp = srv
        .send(
            srv.anon(Method::POST, "/api/guard")
                .header(FORWARDED_URI, "/r/p/s"),
        )
        .await;
    assert_eq!(resp.status, 405);
    assert_eq!(resp.header("location"), None);
}
