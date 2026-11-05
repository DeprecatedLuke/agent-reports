mod common;

use common::{Server, expect_closed, next_event};
use reqwest::Method;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_tungstenite::tungstenite;

fn http_status(err: tungstenite::Error) -> u16 {
    match err {
        tungstenite::Error::Http(resp) => resp.status().as_u16(),
        other => panic!("expected an HTTP handshake rejection, got {other}"),
    }
}

#[tokio::test]
async fn global_ws_requires_owner() {
    let srv = Server::start();
    let err = srv
        .ws("/api/ws", false)
        .await
        .expect_err("handshake must fail");
    assert_eq!(http_status(err), 401);
}

#[tokio::test]
async fn global_ws_streams_report_changes_but_not_noop_puts() {
    let srv = Server::start();
    let mut ws = srv.ws("/api/ws", true).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");

    srv.upsert("p", "s", "T", "<p>one</p>").await;
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["type"], "report.upserted");
    assert_eq!(
        (
            ev["report"]["project"].as_str(),
            ev["report"]["slug"].as_str()
        ),
        (Some("p"), Some("s"))
    );
    assert_eq!(ev["report"]["rev"], 1);

    srv.upsert("p", "s", "T2", "<p>two</p>").await;
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["type"], "report.upserted");
    assert_eq!(ev["report"]["rev"], 2);
    assert_eq!(ev["report"]["title"], "T2");

    // Byte-identical PUT: no event. The very next event must be the delete.
    srv.upsert("p", "s", "T2", "<p>two</p>").await;
    // Rejected PUTs emit nothing either.
    let rejected = srv
        .send(
            srv.owner(Method::PUT, "/api/reports/p/s/source")
                .header("if-match", "\"1\"")
                .body(common::doc("T3", "<p>three</p>")),
        )
        .await;
    assert_eq!(rejected.status, 412);
    srv.delete_report("p", "s").await;
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["type"], "report.deleted");
    assert_eq!(
        (ev["project"].as_str(), ev["slug"].as_str()),
        (Some("p"), Some("s"))
    );
}

#[tokio::test]
async fn share_ws_sees_only_its_report_and_closes_on_revoke() {
    let srv = Server::start();
    srv.upsert("p", "shared", "Shared", "<p>a</p>").await;
    srv.upsert("p", "other", "Other", "<p>b</p>").await;
    let token = srv.share("p", "shared").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = srv.ws(&format!("/api/s/{token}/ws"), false).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");

    srv.upsert("p", "other", "Other", "<p>b2</p>").await;
    srv.upsert("p", "shared", "Shared", "<p>a2</p>").await;
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["type"], "report.upserted");
    assert_eq!(
        ev["report"]["slug"], "shared",
        "foreign report leaked: {ev}"
    );
    assert_eq!(ev["report"]["rev"], 2);

    srv.revoke(&token).await;
    assert_eq!(next_event(&mut ws).await["type"], "share.expired");
    expect_closed(&mut ws).await;

    let err = srv
        .ws(&format!("/api/s/{token}/ws"), false)
        .await
        .expect_err("revoked share must refuse new sockets");
    assert_eq!(http_status(err), 410);
}

#[tokio::test]
async fn share_ws_reports_deletion_and_closes() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>a</p>").await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let mut ws = srv.ws(&format!("/api/s/{token}/ws"), false).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");

    srv.delete_report("p", "s").await;
    let ev = next_event(&mut ws).await;
    assert_eq!(ev["type"], "report.deleted");
    expect_closed(&mut ws).await;
}

#[tokio::test]
async fn share_ws_closes_at_natural_expiry() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>a</p>").await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    // Pull the expiry close: the socket is opened while still active and must
    // announce expiry by itself, without any revoke call.
    let soon = (OffsetDateTime::now_utc() + Duration::seconds(3))
        .format(&Rfc3339)
        .unwrap();
    srv.db()
        .execute(
            "UPDATE shares SET expires_at = ?1 WHERE token = ?2",
            rusqlite::params![soon, token],
        )
        .unwrap();

    let mut ws = srv.ws(&format!("/api/s/{token}/ws"), false).await.unwrap();
    assert_eq!(next_event(&mut ws).await["type"], "hello");
    assert_eq!(next_event(&mut ws).await["type"], "share.expired");
    expect_closed(&mut ws).await;
}

#[tokio::test]
async fn unknown_share_ws_is_404() {
    let srv = Server::start();
    let err = srv
        .ws("/api/s/nope/ws", false)
        .await
        .expect_err("must fail");
    assert_eq!(http_status(err), 404);
}
