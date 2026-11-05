use std::path::PathBuf;

use anyhow::{Result, bail};

/// Runtime configuration, entirely from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub render_dir: PathBuf,
    pub guide_path: PathBuf,
    /// Value of the session cookie (`AGENT_REPORTS_SESSION_TOKEN`).
    pub session_token: String,
    /// Password login credentials (`AGENT_REPORTS_HTPASSWD`); `None` disables it.
    pub htpasswd: Option<Htpasswd>,
    pub public_url: String,
}

/// One `user:bcrypt-hash` htpasswd line.
#[derive(Debug, Clone)]
pub struct Htpasswd {
    pub user: String,
    pub hash: String,
}

impl Htpasswd {
    fn parse(line: &str) -> Result<Self> {
        let Some((user, hash)) = line.split_once(':') else {
            bail!("AGENT_REPORTS_HTPASSWD must be a `user:hash` line");
        };
        if user.is_empty() {
            bail!("AGENT_REPORTS_HTPASSWD has an empty user");
        }
        if !["$2a$", "$2b$", "$2y$"].iter().any(|p| hash.starts_with(p))
            || hash.parse::<bcrypt::HashParts>().is_err()
        {
            bail!("AGENT_REPORTS_HTPASSWD hash must be bcrypt ($2a$, $2b$ or $2y$)");
        }
        Ok(Self {
            user: user.to_string(),
            hash: hash.to_string(),
        })
    }
}

/// Cookie-octet per RFC 6265, so the token can be sent back in `Set-Cookie` verbatim.
fn valid_cookie_value(v: &str) -> bool {
    v.bytes()
        .all(|b| matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E))
}

fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let session_token = std::env::var("AGENT_REPORTS_SESSION_TOKEN").unwrap_or_default();
        let session_token = session_token.trim().to_string();
        if session_token.is_empty() {
            bail!("AGENT_REPORTS_SESSION_TOKEN is required");
        }
        if !valid_cookie_value(&session_token) {
            bail!("AGENT_REPORTS_SESSION_TOKEN must be a valid cookie value");
        }

        let htpasswd = match std::env::var("AGENT_REPORTS_HTPASSWD") {
            Ok(v) if !v.trim().is_empty() => Some(Htpasswd::parse(v.trim())?),
            _ => None,
        };

        let public_url = env_or("AGENT_REPORTS_PUBLIC_URL", "http://localhost:8080");
        let public_url = public_url.trim_end_matches('/').to_string();

        Ok(Self {
            bind: env_or("AGENT_REPORTS_BIND", "0.0.0.0:8080"),
            data_dir: PathBuf::from(env_or("AGENT_REPORTS_DATA_DIR", "/data")),
            render_dir: PathBuf::from(env_or("AGENT_REPORTS_RENDER_DIR", "/app/render")),
            guide_path: PathBuf::from(env_or("AGENT_REPORTS_GUIDE_PATH", "/app/docs/guide.html")),
            session_token,
            htpasswd,
            public_url,
        })
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.data_dir.join("assets")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("agent-reports.db")
    }

    /// Warn (once, at startup) about the missing render directory so the server still boots
    /// in a half-built checkout.
    pub fn warn_missing_dirs(&self) {
        if !self.render_dir.is_dir() {
            tracing::warn!(
                dir = %self.render_dir.display(),
                "render directory missing; related requests will 404 until it is built",
            );
        }
    }
}

/// `^[a-z0-9][a-z0-9_-]{0,63}$`
pub fn valid_ident(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`
pub fn valid_asset_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'_' || *b == b'-')
}

pub fn reserved_project(p: &str) -> bool {
    matches!(p, "list" | "guide")
}

pub fn ensure_ident(p: &str, s: &str) -> Result<(), String> {
    if !valid_ident(p) || !valid_ident(s) {
        return Err("project and slug must match ^[a-z0-9][a-z0-9_-]{0,63}$".into());
    }
    if reserved_project(p) {
        return Err(format!("project `{p}` is reserved"));
    }
    Ok(())
}
