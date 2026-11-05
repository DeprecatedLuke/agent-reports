use std::path::Path;

use sha2::{Digest, Sha256};

use crate::db::ReportRow;

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The `{BODY}` fragment: server-rendered header + the report fragment as served.
/// Fragment for `/body` endpoints: header + fragment derived from the stored source.
pub fn body_html_with_fragment(report: &ReportRow, fragment: &str) -> String {
    let description = if report.description.is_empty() {
        String::new()
    } else {
        let joined = report
            .description
            .split('\n')
            .map(escape)
            .collect::<Vec<_>>()
            .join("<br>");
        format!("<p class=\"report-description\">{joined}</p>")
    };
    format!(
        "<header class=\"report-header\"><div class=\"report-project\">{project}</div><h1 class=\"report-title\">{title}</h1>{description}<div class=\"report-meta\">Updated <time datetime=\"{updated_at}\">{updated_at}</time> · rev {rev}</div></header>\n<main id=\"report-body\" class=\"report-body\">{fragment}</main>",
        project = escape(&report.project),
        title = escape(&report.title),
        description = description,
        updated_at = escape(&report.updated_at),
        rev = report.rev,
        fragment = fragment,
    )
}

/// The full document served at `/api/c/:cap/`.
pub fn document(report: &ReportRow, fragment: &str, cap: &str, version: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"color-scheme\" content=\"light dark\">\n\
<meta name=\"agent-reports-rev\" content=\"{rev}\">\n\
<title>{project}: {title}</title>\n\
<base href=\"/api/c/{cap}/\" target=\"_blank\">\n\
<link rel=\"stylesheet\" href=\"/_render/report.css?v={version}\">\n\
<script type=\"module\" src=\"/_render/runtime.js?v={version}\"></script>\n\
</head><body>{body}</body></html>",
        rev = report.rev,
        project = escape(&report.project),
        title = escape(&report.title),
        cap = cap,
        version = version,
        body = body_html_with_fragment(report, fragment),
    )
}

pub fn csp(public_url: &str, cap: &str) -> String {
    format!(
        "sandbox allow-scripts allow-popups allow-popups-to-escape-sandbox; \
default-src 'none'; \
script-src {P}/_render/ 'unsafe-inline'; \
style-src {P}/_render/ 'unsafe-inline'; \
img-src {P}/api/c/{cap}/assets/ data: blob:; \
media-src {P}/api/c/{cap}/assets/; \
font-src {P}/_render/ data:; \
connect-src {P}/api/c/{cap}/assets/; \
worker-src blob:; \
form-action 'none'; \
base-uri {P}/api/c/{cap}/; \
frame-ancestors {P}",
        P = public_url,
        cap = cap,
    )
}

/// `{v}`: short hash over runtime.js + report.css, computed at startup.
/// Missing files (dev before `render/` is built) fall back to `dev`.
pub fn render_version(render_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    let mut found = false;
    for name in ["runtime.js", "report.css"] {
        let path = render_dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                hasher.update(&bytes);
                found = true;
            }
            Err(_) => {
                tracing::warn!(
                    file = %path.display(),
                    "render asset missing at startup; version hash is partial"
                );
            }
        }
    }
    if !found {
        return "dev".to_string();
    }
    let digest = hex::encode(hasher.finalize());
    digest[..10].to_string()
}
