use std::path::PathBuf;

use anyhow::{Result, bail};

/// Runtime configuration, entirely from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub render_dir: PathBuf,
    pub guide_path: PathBuf,
    pub owner_secret: String,
    pub public_url: String,
}

fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let owner_secret = match std::env::var("AGENT_REPORTS_OWNER_SECRET") {
            Ok(v) if !v.is_empty() => v,
            _ => bail!("AGENT_REPORTS_OWNER_SECRET is required"),
        };

        let public_url = env_or("AGENT_REPORTS_PUBLIC_URL", "http://localhost:8080");
        let public_url = public_url.trim_end_matches('/').to_string();

        Ok(Self {
            bind: env_or("AGENT_REPORTS_BIND", "0.0.0.0:8080"),
            data_dir: PathBuf::from(env_or("AGENT_REPORTS_DATA_DIR", "/data")),
            render_dir: PathBuf::from(env_or("AGENT_REPORTS_RENDER_DIR", "/app/render")),
            guide_path: PathBuf::from(env_or("AGENT_REPORTS_GUIDE_PATH", "/app/docs/guide.html")),
            owner_secret,
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
