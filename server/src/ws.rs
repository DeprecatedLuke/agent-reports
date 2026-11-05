use std::time::Duration as StdDuration;

use axum::{
    Json,
    body::Bytes,
    extract::{
        Path, State,
        ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    App, db,
    db::Db,
    events::{self, Event},
    public::resolve_share,
};

const PING_INTERVAL: StdDuration = StdDuration::from_secs(30);

/// Origin (`scheme://host[:port]`) of a URL; `public_url` has no trailing slash.
fn origin_of(url: &str) -> &str {
    let authority_start = url.find("://").map_or(0, |i| i + 3);
    match url[authority_start..].find('/') {
        Some(i) => &url[..authority_start + i],
        None => url,
    }
}

/// Browsers always send `Origin` on WebSocket upgrades; only the app's own
/// origin may open the owner stream. Clients that send no `Origin` are allowed.
pub async fn global_ws(
    State(app): State<App>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let expected = origin_of(&app.cfg.public_url);
        let ok = origin
            .to_str()
            .is_ok_and(|o| o.trim_end_matches('/').eq_ignore_ascii_case(expected));
        if !ok {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "forbidden origin" })),
            )
                .into_response();
        }
    }
    ws.on_upgrade(move |socket| global_loop(socket, app))
}

async fn send_json(sink: &mut SplitSink<WebSocket, Message>, value: &Value) -> Result<(), ()> {
    sink.send(Message::Text(Utf8Bytes::from(value.to_string())))
        .await
        .map_err(|_| ())
}

async fn global_loop(socket: WebSocket, app: App) {
    let mut rx = app.events.subscribe();
    let (mut sink, mut stream) = socket.split();

    if send_json(&mut sink, &events::hello()).await.is_err() {
        return;
    }

    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if let Some(value) = event.global_json()
                            && send_json(&mut sink, &value).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if send_json(&mut sink, &json!({"type": "resync"})).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}

pub async fn share_ws(
    State(app): State<App>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    match resolve_share(&app, &token).await {
        Ok(share) => {
            let app = app.clone();
            ws.on_upgrade(move |socket| {
                share_loop(
                    socket,
                    app,
                    share.token,
                    share.project,
                    share.slug,
                    share.expires_at,
                )
            })
        }
        Err(m) => crate::public::missing_json_public(m),
    }
}

async fn share_loop(
    socket: WebSocket,
    app: App,
    token: String,
    project: String,
    slug: String,
    expires_at: String,
) {
    let mut rx = app.events.subscribe();
    let (mut sink, mut stream) = socket.split();

    if send_json(&mut sink, &events::hello()).await.is_err() {
        return;
    }

    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let deadline = db::parse_time(&expires_at).unwrap_or_else(db::now);
    let wait = (deadline - db::now()).as_seconds_f64().max(0.0);
    let expiry = tokio::time::sleep(StdDuration::from_secs_f64(wait));
    tokio::pin!(expiry);

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if sink.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
            _ = &mut expiry => {
                let _ = send_json(&mut sink, &json!({"type": "share.expired"})).await;
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if !handle_share_event(&mut sink, event, &token, &project, &slug, &app).await {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        // Fall back to a snapshot so a lagging client still converges.
                        if !resync_share(&mut sink, &app, &project, &slug).await {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Returns `false` when the socket should close.
async fn handle_share_event(
    sink: &mut SplitSink<WebSocket, Message>,
    event: Event,
    token: &str,
    project: &str,
    slug: &str,
    _app: &App,
) -> bool {
    match event {
        Event::ReportUpserted(report) if report.project == project && report.slug == slug => {
            send_json(sink, &json!({"type": "report.upserted", "report": report}))
                .await
                .is_ok()
        }
        Event::ReportDeleted {
            project: p,
            slug: s,
        } if p == project && s == slug => {
            let _ = send_json(
                sink,
                &json!({"type": "report.deleted", "project": p, "slug": s}),
            )
            .await;
            let _ = sink.send(Message::Close(None)).await;
            false
        }
        Event::ShareRevoked { token: t } | Event::ShareExpired { token: t } if t == token => {
            let _ = send_json(sink, &json!({"type": "share.expired"})).await;
            let _ = sink.send(Message::Close(None)).await;
            false
        }
        _ => true,
    }
}

async fn resync_share(
    sink: &mut SplitSink<WebSocket, Message>,
    app: &App,
    project: &str,
    slug: &str,
) -> bool {
    let (p, s) = (project.to_string(), slug.to_string());
    let row = match app.db.call(move |c| Db::get_report(c, &p, &s)).await {
        Ok(row) => row,
        Err(err) => {
            tracing::warn!(error = %err, "resync lookup failed");
            return true;
        }
    };
    match row {
        Some(report) => {
            let summary = db::ReportSummary::from(&report);
            send_json(sink, &json!({"type": "report.upserted", "report": summary}))
                .await
                .is_ok()
        }
        None => {
            let _ = send_json(
                sink,
                &json!({"type": "report.deleted", "project": project, "slug": slug}),
            )
            .await;
            let _ = sink.send(Message::Close(None)).await;
            false
        }
    }
}
