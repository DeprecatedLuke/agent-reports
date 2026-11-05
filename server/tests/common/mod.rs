//! Black-box harness: every test spawns the real `agent-reports` binary on an ephemeral
//! port with its own temp data/render directories, then talks HTTP/WebSocket to it.
#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{Method, RequestBuilder, header::HeaderMap};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{self, Message, client::IntoClientRequest, http::HeaderValue},
};

pub const OWNER: &str = "test-owner-secret";
pub const PUBLIC_URL: &str = "http://public.test";
pub const RUNTIME_JS: &str = "/* runtime fixture */";

const WAIT: Duration = Duration::from_secs(10);

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Server {
    child: Child,
    base: String,
    data_dir: PathBuf,
    client: reqwest::Client,
    _dir: tempfile::TempDir,
}

pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("response body is UTF-8")
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "expected JSON body (status {}): {err}: {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(name)
            .map(|v| v.to_str().expect("ASCII header"))
    }
}

/// Wraps a report body in the minimal valid frontmatter.
pub fn doc(title: &str, body: &str) -> String {
    format!("---\ntitle: {title}\n---\n{body}")
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn listening_addr(line: &str) -> Option<SocketAddr> {
    let line = strip_ansi(line);
    if !line.contains("listening") {
        return None;
    }
    let rest = &line[line.find("addr=")? + "addr=".len()..];
    rest.split_whitespace().next()?.parse().ok()
}

impl Server {
    /// Server without a render-dir `icons.json` (the dev case: icon names are not checked).
    pub fn start() -> Self {
        Self::spawn(None)
    }

    /// Server whose render dir has an `icons.json` (as emitted by the render build) that
    /// knows exactly `names`.
    pub fn start_with_icons(names: &[&str]) -> Self {
        let icons: serde_json::Map<String, Value> = names
            .iter()
            .map(|name| {
                let svg = format!(r#"<svg viewBox="0 0 20 20"><title>{name}</title></svg>"#);
                (name.to_string(), Value::String(svg))
            })
            .collect();
        let json = serde_json::json!({ "set": "20/solid", "version": "2.2.0", "icons": icons });
        Self::spawn(Some(json.to_string()))
    }

    fn spawn(icons_json: Option<String>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let render = dir.path().join("render");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&render).unwrap();
        std::fs::write(render.join("runtime.js"), RUNTIME_JS).unwrap();
        std::fs::write(render.join("report.css"), "/* css fixture */").unwrap();
        if let Some(json) = icons_json {
            std::fs::write(render.join("icons.json"), json).unwrap();
        }

        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-reports"))
            .env_clear()
            .env("AGENT_REPORTS_BIND", "127.0.0.1:0")
            .env("AGENT_REPORTS_DATA_DIR", &data_dir)
            .env("AGENT_REPORTS_RENDER_DIR", &render)
            .env("AGENT_REPORTS_GUIDE_PATH", dir.path().join("no-guide.html"))
            .env("AGENT_REPORTS_OWNER_SECRET", OWNER)
            .env("AGENT_REPORTS_PUBLIC_URL", PUBLIC_URL)
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawning agent-reports binary");

        // Keep draining stdout for the whole run so the server never blocks on a full pipe.
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut tx = Some(tx);
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(addr) = listening_addr(&line)
                    && let Some(tx) = tx.take()
                {
                    let _ = tx.send(addr);
                }
            }
        });
        let addr = match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(addr) => addr,
            Err(err) => {
                let _ = child.kill();
                panic!("server never reported its listen address: {err}");
            }
        };

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            child,
            base: format!("http://{addr}"),
            data_dir,
            client,
            _dir: dir,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Request carrying the correct owner header.
    pub fn owner(&self, method: Method, path: &str) -> RequestBuilder {
        self.anon(method, path)
            .header("x-agent-reports-owner", OWNER)
    }

    /// Request without any owner header (what Traefik forwards on public routes).
    pub fn anon(&self, method: Method, path: &str) -> RequestBuilder {
        self.client.request(method, self.url(path))
    }

    /// Sends and buffers a response. Every response is checked for the
    /// "server never sets cookies" invariant.
    pub async fn send(&self, req: RequestBuilder) -> Resp {
        let resp = req.send().await.expect("HTTP request failed");
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        assert!(
            headers.get("set-cookie").is_none(),
            "server emitted Set-Cookie on {}",
            resp.url()
        );
        let body = resp.bytes().await.expect("reading body").to_vec();
        Resp {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.send(self.owner(Method::GET, path)).await
    }

    pub async fn get_anon(&self, path: &str) -> Resp {
        self.send(self.anon(Method::GET, path)).await
    }

    pub async fn put_source(&self, project: &str, slug: &str, source: &str) -> Resp {
        let path = format!("/api/reports/{project}/{slug}/source");
        self.send(self.owner(Method::PUT, &path).body(source.to_string()))
            .await
    }

    /// Creates/updates a report and returns its `ReportSummary`.
    pub async fn upsert(&self, project: &str, slug: &str, title: &str, body: &str) -> Value {
        let resp = self.put_source(project, slug, &doc(title, body)).await;
        assert_eq!(resp.status, 200, "PUT failed: {}", resp.text());
        resp.json()["report"].clone()
    }

    pub async fn delete_report(&self, project: &str, slug: &str) {
        let path = format!("/api/reports/{project}/{slug}");
        let resp = self.send(self.owner(Method::DELETE, &path)).await;
        assert_eq!(resp.status, 204, "DELETE failed: {}", resp.text());
    }

    /// Creates a share and returns the `Share` JSON.
    pub async fn share(&self, project: &str, slug: &str) -> Value {
        let path = format!("/api/reports/{project}/{slug}/shares");
        let resp = self.send(self.owner(Method::POST, &path)).await;
        assert_eq!(resp.status, 201, "share failed: {}", resp.text());
        resp.json()
    }

    pub async fn revoke(&self, token: &str) {
        let resp = self
            .send(self.owner(Method::DELETE, &format!("/api/shares/{token}")))
            .await;
        assert_eq!(resp.status, 204, "revoke failed: {}", resp.text());
    }

    pub async fn view_cap(&self, project: &str, slug: &str) -> String {
        let path = format!("/api/reports/{project}/{slug}/view-cap");
        let resp = self.send(self.owner(Method::POST, &path)).await;
        assert_eq!(resp.status, 200, "view-cap failed: {}", resp.text());
        resp.json()["cap"].as_str().unwrap().to_string()
    }

    pub async fn put_asset(
        &self,
        project: &str,
        slug: &str,
        name: &str,
        content_type: Option<&str>,
        bytes: Vec<u8>,
    ) -> Resp {
        let path = format!("/api/reports/{project}/{slug}/assets/{name}");
        let mut req = self.owner(Method::PUT, &path).body(bytes);
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        self.send(req).await
    }

    /// A second connection to the server's SQLite file, for moving time (expiry) in tests.
    pub fn db(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(self.data_dir.join("agent-reports.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        conn
    }

    pub async fn ws(&self, path: &str, with_owner: bool) -> Result<Ws, tungstenite::Error> {
        self.ws_with_origin(path, with_owner, None).await
    }

    pub async fn ws_with_origin(
        &self,
        path: &str,
        with_owner: bool,
        origin: Option<&str>,
    ) -> Result<Ws, tungstenite::Error> {
        let url = format!("{}{path}", self.base.replacen("http://", "ws://", 1));
        let mut req = url.into_client_request()?;
        if with_owner {
            req.headers_mut()
                .insert("x-agent-reports-owner", HeaderValue::from_static(OWNER));
        }
        if let Some(origin) = origin {
            req.headers_mut()
                .insert("origin", HeaderValue::from_str(origin).unwrap());
        }
        let (ws, _) = tokio_tungstenite::connect_async(req).await?;
        Ok(ws)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Next JSON event on a websocket (control frames skipped). Panics on close or after
/// a generous deadline, so a missing event fails instead of hanging.
pub async fn next_event(ws: &mut Ws) -> Value {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str::<Value>(&text).expect("event is JSON");
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                other => panic!("expected a JSON event, got {other:?}"),
            }
        }
    };
    tokio::time::timeout(WAIT, fut)
        .await
        .expect("timed out waiting for websocket event")
}

/// Asserts the server closes the socket next (control frames skipped).
pub async fn expect_closed(ws: &mut Ws) {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(_))) | None => return,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Err(_)) => return,
                Some(Ok(other)) => panic!("expected close, got {other:?}"),
            }
        }
    };
    tokio::time::timeout(WAIT, fut)
        .await
        .expect("timed out waiting for websocket close");
}
