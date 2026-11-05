mod common;

use common::{PUBLIC_URL, Server};
use reqwest::Method;
use serde_json::Value;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

fn ts(v: &Value) -> OffsetDateTime {
    OffsetDateTime::parse(v.as_str().expect("timestamp string"), &Rfc3339).expect("RFC3339")
}

fn tokens(shares: &Value) -> Vec<String> {
    shares["shares"]
        .as_array()
        .or_else(|| shares.as_array())
        .expect("shares array")
        .iter()
        .map(|s| s["token"].as_str().unwrap().to_string())
        .collect()
}

/// Every public JSON surface of a share token: meta, body, and document. (The share
/// shell is served by nginx, so the server no longer 410s/404s it.)
const SHARE_SURFACES: [&str; 3] = ["/api/s/{}/meta", "/api/s/{}/body", "/api/c/{}/"];

async fn share_statuses(srv: &Server, token: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tpl in SHARE_SURFACES {
        out.push(srv.get_anon(&tpl.replace("{}", token)).await.status);
    }
    out
}

/// Flip one character in the middle of a capability string.
fn tamper(cap: &str) -> String {
    let mut chars: Vec<char> = cap.chars().collect();
    let mut i = chars.len() / 2;
    while chars[i] == '.' {
        i += 1;
    }
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

#[tokio::test]
async fn each_share_gets_a_fresh_token_expiring_exactly_seven_days_later() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>x</p>").await;

    let a = srv.share("p", "s").await;
    let b = srv.share("p", "s").await;
    assert_ne!(a["token"], b["token"]);
    for share in [&a, &b] {
        assert_eq!(
            ts(&share["expires_at"]) - ts(&share["created_at"]),
            Duration::days(7)
        );
        assert_eq!(
            share["url"],
            format!("{PUBLIC_URL}/s/{}", share["token"].as_str().unwrap())
        );
    }

    let mut listed = tokens(&srv.get("/api/reports/p/s/shares").await.json());
    listed.sort();
    let mut expected = tokens(&Value::Array(vec![a.clone(), b.clone()]));
    expected.sort();
    assert_eq!(listed, expected);

    let meta = srv
        .get_anon(&format!("/api/s/{}/meta", a["token"].as_str().unwrap()))
        .await
        .json();
    assert_eq!(meta["expires_at"], a["expires_at"]);
    assert_eq!(
        (meta["project"].as_str(), meta["slug"].as_str()),
        (Some("p"), Some("s"))
    );
}

#[tokio::test]
async fn sharing_a_missing_report_is_404() {
    let srv = Server::start();
    let resp = srv
        .send(srv.owner(Method::POST, "/api/reports/p/nope/shares"))
        .await;
    assert_eq!(resp.status, 404);
}

#[tokio::test]
async fn revoked_share_is_gone_on_every_public_surface() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>x</p>").await;
    let revoked = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let kept = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(share_statuses(&srv, &revoked).await, [200; 3]);

    srv.revoke(&revoked).await;

    assert_eq!(share_statuses(&srv, &revoked).await, [410; 3]);
    assert_eq!(
        share_statuses(&srv, &kept).await,
        [200; 3],
        "other shares unaffected"
    );
    assert_eq!(
        tokens(&srv.get("/api/reports/p/s/shares").await.json()),
        [kept.as_str()]
    );
    assert_eq!(
        tokens(&srv.get("/api/reports/p/s").await.json()),
        [kept.as_str()]
    );
}

#[tokio::test]
async fn expired_share_is_gone() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>x</p>").await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let past = (OffsetDateTime::now_utc() - Duration::seconds(1))
        .format(&Rfc3339)
        .unwrap();
    let changed = srv
        .db()
        .execute(
            "UPDATE shares SET expires_at = ?1 WHERE token = ?2",
            rusqlite::params![past, token],
        )
        .unwrap();
    assert_eq!(changed, 1);

    assert_eq!(share_statuses(&srv, &token).await, [410; 3]);
    assert!(tokens(&srv.get("/api/reports/p/s/shares").await.json()).is_empty());
}

#[tokio::test]
async fn unknown_share_token_is_404() {
    let srv = Server::start();
    assert_eq!(
        share_statuses(&srv, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").await,
        [404; 3]
    );
}

#[tokio::test]
async fn deleting_a_report_kills_its_shares() {
    let srv = Server::start();
    srv.upsert("p", "s", "T", "<p>x</p>").await;
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    srv.delete_report("p", "s").await;
    assert_eq!(srv.get("/api/reports/p/s").await.status, 404);
    for status in share_statuses(&srv, &token).await {
        assert!(
            status == 404 || status == 410,
            "share survived delete: {status}"
        );
    }

    // Recreating the report must not resurrect the old share.
    srv.upsert("p", "s", "T", "<p>x</p>").await;
    for status in share_statuses(&srv, &token).await {
        assert!(
            status == 404 || status == 410,
            "share resurrected: {status}"
        );
    }
}

#[tokio::test]
async fn view_cap_serves_sandboxed_document() {
    let srv = Server::start();
    srv.upsert("p", "s", "Capped", "<p>cap-body-marker</p>")
        .await;
    let cap = srv.view_cap("p", "s").await;

    let redirect = srv.get_anon(&format!("/api/c/{cap}")).await;
    assert_eq!(redirect.status, 308);
    assert_eq!(
        redirect.header("location"),
        Some(format!("/api/c/{cap}/").as_str())
    );

    let resp = srv.get_anon(&format!("/api/c/{cap}/")).await;
    assert_eq!(resp.status, 200);
    let html = resp.text();
    assert!(html.contains("cap-body-marker"));
    let csp = resp.header("content-security-policy").expect("CSP header");
    let sandbox: Vec<&str> = csp
        .split(';')
        .map(str::trim)
        .find_map(|d| d.strip_prefix("sandbox"))
        .expect("sandbox directive")
        .split_whitespace()
        .collect();
    assert!(sandbox.contains(&"allow-scripts"), "{csp}");
    assert!(!sandbox.contains(&"allow-same-origin"), "{csp}");
    assert!(!csp.contains("allow-same-origin"), "{csp}");
    assert!(
        csp.contains(&format!("img-src {PUBLIC_URL}/api/c/{cap}/assets/")),
        "images must be scoped to this cap: {csp}"
    );
    assert!(
        csp.contains(&format!("base-uri {PUBLIC_URL}/api/c/{cap}/;")),
        "base-uri must allow this cap's assets: {csp}"
    );
    // The runtime assets are served by the static image on the same origin.
    assert!(html.contains("/_render/report.css?v="), "stylesheet");
    assert!(html.contains("/_render/runtime.js?v="), "runtime");
    let base = format!("<base href=\"/api/c/{cap}/\" target=\"_blank\">");
    assert!(
        html.contains(&base),
        "relative asset URLs resolve against the cap"
    );

    // Share tokens are caps too, with the same sandbox.
    let token = srv.share("p", "s").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let shared = srv.get_anon(&format!("/api/c/{token}/")).await;
    assert_eq!(shared.status, 200);
    let csp = shared.header("content-security-policy").unwrap();
    assert!(csp.starts_with("sandbox allow-scripts") && !csp.contains("allow-same-origin"));
}

#[tokio::test]
async fn tampered_or_spliced_caps_are_rejected() {
    let srv = Server::start();
    srv.upsert("p", "a", "A", "<p>report-a-marker</p>").await;
    srv.upsert("p", "b", "B", "<p>report-b-marker</p>").await;
    let cap_a = srv.view_cap("p", "a").await;
    let cap_b = srv.view_cap("p", "b").await;
    assert_eq!(srv.get_anon(&format!("/api/c/{cap_a}/")).await.status, 200);

    // B's payload with A's signature (and vice versa).
    let (payload_b, _) = cap_b.rsplit_once('.').unwrap();
    let (_, tag_a) = cap_a.rsplit_once('.').unwrap();
    let spliced = format!("{payload_b}.{tag_a}");

    for bad in [
        tamper(&cap_a),
        spliced,
        cap_a[..cap_a.len() - 2].to_string(),
        format!("{cap_a}.x"),
    ] {
        let resp = srv.get_anon(&format!("/api/c/{bad}/")).await;
        assert!(
            (400..500).contains(&resp.status),
            "{bad} -> {}",
            resp.status
        );
        let body = resp.text();
        assert!(!body.contains("report-a-marker") && !body.contains("report-b-marker"));
    }
}

#[tokio::test]
async fn assets_are_served_through_caps_with_cors_and_svg_sandbox() {
    let srv = Server::start();
    let png: Vec<u8> = (0..=255u8).collect();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>".to_vec();

    // Uploading before the report exists is allowed.
    let up = srv
        .put_asset("p", "s", "a.png", Some("image/png"), png.clone())
        .await;
    assert_eq!(up.status, 200, "{}", up.text());
    assert_eq!(up.json()["src"], "assets/a.png");
    assert_eq!(up.json()["size"], 256);
    let up = srv
        .put_asset(
            "p",
            "s",
            "d.svg",
            Some("image/svg+xml; charset=utf-8"),
            svg.clone(),
        )
        .await;
    assert_eq!(up.status, 200, "{}", up.text());

    srv.upsert("p", "s", "T", "<img src=\"assets/a.png\">")
        .await;
    let cap = srv.view_cap("p", "s").await;

    let resp = srv.get_anon(&format!("/api/c/{cap}/assets/a.png")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, png);
    assert_eq!(resp.header("content-type"), Some("image/png"));
    assert_eq!(resp.header("access-control-allow-origin"), Some("*"));
    assert_eq!(resp.header("content-security-policy"), None);

    let resp = srv.get_anon(&format!("/api/c/{cap}/assets/d.svg")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, svg);
    assert_eq!(resp.header("content-type"), Some("image/svg+xml"));
    assert_eq!(resp.header("access-control-allow-origin"), Some("*"));
    assert_eq!(resp.header("content-security-policy"), Some("sandbox"));

    assert_eq!(
        srv.get_anon(&format!("/api/c/{cap}/assets/missing.png"))
            .await
            .status,
        404
    );

    // Re-uploading replaces the bytes served under the same name.
    let replaced = vec![9u8; 10];
    srv.put_asset("p", "s", "a.png", Some("image/png"), replaced.clone())
        .await;
    assert_eq!(
        srv.get_anon(&format!("/api/c/{cap}/assets/a.png"))
            .await
            .body,
        replaced
    );

    // Deleting the report removes its assets.
    srv.delete_report("p", "s").await;
    srv.upsert("p", "s", "T", "<p>again</p>").await;
    let cap = srv.view_cap("p", "s").await;
    assert_eq!(
        srv.get_anon(&format!("/api/c/{cap}/assets/a.png"))
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn a_cap_only_reaches_its_own_reports_assets() {
    let srv = Server::start();
    srv.upsert("p", "a", "A", "<p>a</p>").await;
    srv.upsert("p", "b", "B", "<p>b</p>").await;
    srv.put_asset("p", "a", "same.png", Some("image/png"), b"from-a".to_vec())
        .await;
    srv.put_asset("p", "b", "same.png", Some("image/png"), b"from-b".to_vec())
        .await;
    srv.put_asset(
        "p",
        "b",
        "only-b.png",
        Some("image/png"),
        b"secret-b".to_vec(),
    )
    .await;

    let cap_a = srv.view_cap("p", "a").await;
    let share_a = srv.share("p", "a").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    for cap in [cap_a, share_a] {
        let same = srv.get_anon(&format!("/api/c/{cap}/assets/same.png")).await;
        assert_eq!(same.body, b"from-a");
        let other = srv
            .get_anon(&format!("/api/c/{cap}/assets/only-b.png"))
            .await;
        assert_eq!(other.status, 404);
        assert!(!other.text().contains("secret-b"));
    }
}

#[tokio::test]
async fn asset_upload_rejects_bad_names_and_types() {
    let srv = Server::start();
    let name_128 = format!("a{}", "b".repeat(127));
    let name_129 = format!("a{}", "b".repeat(128));
    let cases: [(&str, Option<&str>, u16); 9] = [
        (".hidden", Some("image/png"), 400),
        ("-dash.png", Some("image/png"), 400),
        ("sp%20ace.png", Some("image/png"), 400),
        ("%C3%BC.png", Some("image/png"), 400),
        (&name_129, Some("image/png"), 400),
        ("page.html", Some("text/html"), 415),
        ("app.js", Some("application/javascript"), 415),
        ("no-type.png", None, 415),
        (&name_128, Some("text/csv"), 200),
    ];
    for (name, ct, status) in cases {
        let resp = srv
            .put_asset("p", "s", name, ct, b"a,b\n1,2\n".to_vec())
            .await;
        assert_eq!(resp.status, status, "{name} {ct:?}: {}", resp.text());
    }
    let listed = srv.get("/api/reports/p/s/assets").await.json();
    let names: Vec<&str> = listed["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [name_128.as_str()]);
}

#[tokio::test]
async fn asset_size_limit_is_25_mib() {
    let srv = Server::start();
    let limit = 25 * 1024 * 1024;
    let at = srv
        .put_asset("p", "s", "at.bin.png", Some("image/png"), vec![1u8; limit])
        .await;
    assert_eq!(at.status, 200, "{}", at.text());
    assert_eq!(at.json()["size"], limit);
    let over = srv
        .put_asset(
            "p",
            "s",
            "over.png",
            Some("image/png"),
            vec![1u8; limit + 1],
        )
        .await;
    assert_eq!(over.status, 413);
    assert_eq!(
        srv.get("/api/reports/p/s/assets/over.png").await.status,
        404
    );
}
