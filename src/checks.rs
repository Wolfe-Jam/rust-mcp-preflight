//! The preflight checks. Each returns a `Report`: one line per finding.
//!
//! The README check follows the registry's cargo validator
//! (`internal/validators/registries/cargo.go`): crates.io readme metadata,
//! then the rendered HTML on static.crates.io, then the token match.

use std::fmt::Write as _;
use std::time::Duration;

use serde_json::Value;

use crate::token;

const CRATES_IO: &str = "https://crates.io";
const REGISTRY: &str = "https://registry.modelcontextprotocol.io";
const USER_AGENT: &str = concat!(
    "rust-mcp-preflight/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/Wolfe-Jam/rust-mcp-preflight)"
);
const MAX_README_BYTES: usize = 5 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Warn,
    Info,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Warn => "WARN",
            Status::Info => "INFO",
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<(Status, String)>,
}

impl Report {
    fn push(&mut self, status: Status, msg: impl Into<String>) {
        self.lines.push((status, msg.into()));
    }
    pub fn failed(&self) -> bool {
        self.lines.iter().any(|(s, _)| *s == Status::Fail)
    }
    fn extend(&mut self, other: Report) {
        self.lines.extend(other.lines);
    }
    pub fn render(&self, title: &str) -> String {
        let mut out = format!("{title}\n");
        for (s, m) in &self.lines {
            let _ = writeln!(out, "  {}  {m}", s.label());
        }
        let fails = self
            .lines
            .iter()
            .filter(|(s, _)| *s == Status::Fail)
            .count();
        let warns = self
            .lines
            .iter()
            .filter(|(s, _)| *s == Status::Warn)
            .count();
        let _ = write!(
            out,
            "\n{}",
            if fails == 0 {
                format!("Ready: no failures, {warns} warning(s).")
            } else {
                format!("Not ready: {fails} failure(s), {warns} warning(s).")
            }
        );
        out
    }
}

/// An HTTP client that, like the registry's, only follows redirects to
/// crates.io, static.crates.io or the registry itself, over https.
fn client() -> reqwest::Client {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let ok = attempt.url().scheme() == "https"
            && matches!(
                attempt.url().host_str(),
                Some("crates.io" | "static.crates.io" | "registry.modelcontextprotocol.io")
            );
        if !ok || attempt.previous().len() >= 10 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .redirect(policy)
        .build()
        .expect("http client")
}

fn transient(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

// ─── server.json (offline) ──────────────────────────────────────────────

/// Checks a server.json's text against the registry's rules for cargo
/// packages. Offline: nothing is fetched.
pub fn check_server_json(text: &str) -> Report {
    let mut r = Report::default();
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            r.push(Status::Fail, format!("server.json is not valid JSON: {e}"));
            return r;
        }
    };

    // Placeholders `mcp-publisher init` writes, still unedited.
    const PLACEHOLDERS: [&str; 6] = [
        "com.example/",
        "[describe what your server does]",
        "@your-org/your-package",
        "@YOUR_USERNAME/YOUR_REPO",
        "YOUR_API_KEY",
        "YOUR_USERNAME",
    ];
    for ph in PLACEHOLDERS {
        if text.contains(ph) {
            r.push(
                Status::Fail,
                format!("still has the mcp-publisher init placeholder {ph:?}: replace it with your own value"),
            );
        }
    }

    if v.get("$schema").and_then(Value::as_str).is_none() {
        r.push(
            Status::Warn,
            "no \"$schema\": add the registry schema URL so editors and mcp-publisher validate can check the shape",
        );
    }

    let name = v.get("name").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        r.push(Status::Fail, "\"name\" is missing");
    } else if token::is_valid_server_name(name) {
        r.push(
            Status::Pass,
            format!("name \"{name}\" fits the pattern namespace/name"),
        );
    } else {
        r.push(
            Status::Fail,
            format!("name \"{name}\" doesn't fit ^[a-zA-Z0-9.-]+/[a-zA-Z0-9._-]+$"),
        );
    }

    let top_version = v.get("version").and_then(Value::as_str);
    if top_version.is_none() {
        r.push(Status::Fail, "top-level \"version\" is missing");
    }

    let packages = v.get("packages").and_then(Value::as_array);
    let Some(packages) = packages.filter(|p| !p.is_empty()) else {
        if v.get("remotes")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            r.push(
                Status::Info,
                "no packages, remotes only: nothing for the cargo checks",
            );
        } else {
            r.push(
                Status::Fail,
                "no \"packages\" (and no \"remotes\"): nothing to list",
            );
        }
        return r;
    };

    if !packages
        .iter()
        .any(|p| p.get("registryType").and_then(Value::as_str) == Some("cargo"))
    {
        r.push(
            Status::Warn,
            "no cargo package. In a Rust repo, mcp-publisher init writes an npm entry (it reads package.json only): set registryType to \"cargo\" and identifier to your crate name",
        );
    }

    for (i, p) in packages.iter().enumerate() {
        let kind = p.get("registryType").and_then(Value::as_str).unwrap_or("");
        let id = p.get("identifier").and_then(Value::as_str).unwrap_or("");
        let at = format!("packages[{i}]");
        if kind != "cargo" {
            r.push(
                Status::Info,
                format!("{at}: registryType \"{kind}\" isn't checked by this version (cargo only)"),
            );
            continue;
        }
        if id.is_empty() {
            r.push(
                Status::Fail,
                format!("{at}: \"identifier\" (the crate name) is missing"),
            );
        } else {
            r.push(Status::Pass, format!("{at}: cargo crate \"{id}\""));
        }
        match p.get("version").and_then(Value::as_str) {
            None | Some("") => r.push(
                Status::Fail,
                format!("{at}: \"version\" is required for cargo: it says which crate version's README is checked"),
            ),
            Some(pv) => {
                if top_version.is_some_and(|t| t != pv) {
                    r.push(
                        Status::Warn,
                        format!(
                            "{at}: version {pv} differs from the top-level version {}; keep them equal",
                            top_version.unwrap_or("")
                        ),
                    );
                }
            }
        }
        if p.get("fileSha256").is_some() {
            r.push(
                Status::Fail,
                format!("{at}: remove \"fileSha256\": the registry rejects it on cargo packages (it's for mcpb only)"),
            );
        }
        if let Some(base) = p.get("registryBaseUrl").and_then(Value::as_str)
            && base != CRATES_IO
        {
            r.push(
                Status::Fail,
                format!("{at}: registryBaseUrl must be {CRATES_IO} for cargo, not {base}"),
            );
        }
        if p.get("runtimeHint").is_some() {
            r.push(
                Status::Warn,
                format!("{at}: drop \"runtimeHint\": cargo has no per-run runner (users cargo install once)"),
            );
        }
        match p.pointer("/transport/type").and_then(Value::as_str) {
            Some("stdio") => {}
            Some(t) => r.push(
                Status::Info,
                format!("{at}: transport \"{t}\" (most cargo servers use stdio)"),
            ),
            None => r.push(
                Status::Fail,
                format!("{at}: \"transport\" is missing (usually {{\"type\": \"stdio\"}})"),
            ),
        }
    }
    r
}

// ─── README token on crates.io ──────────────────────────────────────────

/// Fetches the README crates.io serves for this exact crate version and looks
/// for `mcp-name: <server_name>`, as the registry does at publish.
pub async fn check_readme(crate_name: &str, version: &str, server_name: &str) -> Report {
    let mut r = Report::default();
    let http = client();
    let id = urlencode(crate_name);
    let ver = urlencode(version);

    let meta_url = format!("{CRATES_IO}/api/v1/crates/{id}/{ver}/readme");
    let meta = match http
        .get(&meta_url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            r.push(Status::Fail, format!("couldn't reach crates.io: {e}"));
            return r;
        }
    };
    let status = meta.status().as_u16();
    if status != 200 {
        if transient(status) {
            r.push(Status::Warn, format!("crates.io returned {status} for the readme metadata: likely temporary, try again"));
        } else if status == 404 {
            r.push(
                Status::Fail,
                format!(
                    "{crate_name} {version} not found on crates.io: cargo publish first, then list"
                ),
            );
        } else {
            r.push(
                Status::Fail,
                format!("crates.io readme metadata failed ({status})"),
            );
        }
        return r;
    }
    let readme_url = match meta
        .json::<Value>()
        .await
        .ok()
        .and_then(|v| v["url"].as_str().map(String::from))
    {
        Some(u) => u,
        None => {
            r.push(Status::Fail, "crates.io's readme metadata had no \"url\"");
            return r;
        }
    };
    if !readme_url.starts_with("https://static.crates.io/")
        && !readme_url.starts_with("https://crates.io/")
    {
        r.push(
            Status::Fail,
            format!("crates.io returned an unexpected README URL: {readme_url}"),
        );
        return r;
    }

    let resp = match http
        .get(&readme_url)
        .header("Accept", "text/html")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            r.push(
                Status::Fail,
                format!("couldn't fetch the rendered README: {e}"),
            );
            return r;
        }
    };
    let status = resp.status().as_u16();
    if status == 403 {
        // Like the registry: a 403 means no README, or no such version. Ask which.
        let probe = format!("{CRATES_IO}/api/v1/crates/{id}/{ver}");
        let exists = http
            .get(&probe)
            .header("Accept", "application/json")
            .send()
            .await
            .map(|r| r.status().as_u16());
        match exists {
            Ok(404) => r.push(Status::Fail, format!("{crate_name} {version} not found on crates.io: cargo publish first, then list")),
            Ok(s) if transient(s) => r.push(Status::Warn, format!("crates.io couldn't confirm {crate_name} {version} ({s}): likely temporary, try again")),
            _ => r.push(
                Status::Fail,
                format!("{crate_name} {version} has no rendered README (403). Add a README containing `mcp-name: {server_name}` and publish a new version"),
            ),
        }
        return r;
    }
    if status != 200 {
        let s = if transient(status) {
            Status::Warn
        } else {
            Status::Fail
        };
        r.push(s, format!("rendered README fetch returned {status}"));
        return r;
    }
    let mut body = resp.text().await.unwrap_or_default();
    if body.len() > MAX_README_BYTES {
        let mut cut = MAX_README_BYTES;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }

    if token::contains_token(&body, server_name) {
        r.push(
            Status::Pass,
            format!(
                "README of {crate_name} {version} on crates.io contains `mcp-name: {server_name}`"
            ),
        );
        return r;
    }
    if let Some(c) = token::glued_trailing(&body, server_name) {
        r.push(
            Status::Fail,
            format!("found `mcp-name: {server_name}` but it runs straight into {c:?}. Put it on its own line and publish a new version"),
        );
        return r;
    }
    r.push(
        Status::Fail,
        format!("README of {crate_name} {version} doesn't contain `mcp-name: {server_name}`. A README is fixed at cargo publish: add the line as visible text (crates.io strips HTML comments) and publish a new version"),
    );
    let near = token::names_found(&body);
    if !near.is_empty() {
        r.push(
            Status::Info,
            format!("mcp-name values that are there: {}", near.join(", ")),
        );
    }
    r
}

// ─── Server name in the registry ────────────────────────────────────────

/// Checks the name's pattern, whether it's already listed, and what login
/// its namespace needs.
pub async fn check_name(server_name: &str) -> Report {
    let mut r = Report::default();
    if !token::is_valid_server_name(server_name) {
        r.push(
            Status::Fail,
            format!("\"{server_name}\" doesn't fit ^[a-zA-Z0-9.-]+/[a-zA-Z0-9._-]+$"),
        );
        return r;
    }
    let (ns, _) = server_name.split_once('/').unwrap_or_default();
    let http = client();

    if let Some(owner) = ns.strip_prefix("io.github.") {
        let url = format!("https://api.github.com/users/{}", urlencode(owner));
        let kind = match http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) if resp.status().as_u16() == 404 => Some("missing".to_string()),
            Ok(resp) if resp.status().is_success() => resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["type"].as_str().map(String::from)),
            _ => None,
        };
        match kind.as_deref() {
            Some("missing") => r.push(Status::Fail, format!("no GitHub account or organization named {owner}")),
            Some("Organization") => r.push(
                Status::Warn,
                format!("{owner} is a GitHub organization: `mcp-publisher login github` can't publish to org namespaces yet (registry #1468). Be an org Owner, then publish from GitHub Actions or log in with a token that has read:org"),
            ),
            _ => r.push(
                Status::Info,
                format!("namespace io.github.{owner}: publish with `mcp-publisher login github` as {owner}"),
            ),
        }
    } else {
        r.push(
            Status::Info,
            format!("namespace {ns}: prove the domain with `mcp-publisher login dns` (or http)"),
        );
    }

    let url = format!(
        "{REGISTRY}/v0.1/servers?search={}&version=latest",
        urlencode(server_name)
    );
    match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let v: Value = resp.json().await.unwrap_or_default();
            let hit = v["servers"].as_array().and_then(|a| {
                a.iter()
                    .find(|s| s["server"]["name"].as_str() == Some(server_name))
            });
            match hit {
                Some(s) => {
                    let ver = s["server"]["version"].as_str().unwrap_or("?");
                    let st = s["_meta"]["io.modelcontextprotocol.registry/official"]["status"]
                        .as_str()
                        .unwrap_or("?");
                    r.push(
                        Status::Info,
                        format!("already listed: latest {ver} ({st}). Your next publish needs a new version number"),
                    );
                }
                None => r.push(
                    Status::Pass,
                    format!("{server_name} isn't listed yet: first publish"),
                ),
            }
        }
        Ok(resp) => r.push(
            Status::Warn,
            format!("registry search returned {}", resp.status()),
        ),
        Err(e) => r.push(Status::Warn, format!("couldn't reach the registry: {e}")),
    }
    r
}

// ─── All of it ──────────────────────────────────────────────────────────

/// server.json, then the name, then each cargo package's README.
pub async fn preflight(server_json: &str) -> Report {
    let mut r = check_server_json(server_json);
    let Ok(v) = serde_json::from_str::<Value>(server_json) else {
        return r;
    };
    let name = v["name"].as_str().unwrap_or("").to_string();
    if name.is_empty() {
        return r;
    }
    r.extend(check_name(&name).await);
    if let Some(packages) = v["packages"].as_array() {
        for p in packages {
            if p["registryType"].as_str() != Some("cargo") {
                continue;
            }
            if let (Some(id), Some(ver)) = (p["identifier"].as_str(), p["version"].as_str()) {
                r.extend(check_readme(id, ver, &name).await);
            }
        }
    }
    r
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses(r: &Report) -> Vec<Status> {
        r.lines.iter().map(|(s, _)| *s).collect()
    }

    const GOOD: &str = r#"{
      "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
      "name": "io.github.you/widget",
      "description": "A Rust MCP server for widgets",
      "version": "0.3.0",
      "packages": [{"registryType": "cargo", "identifier": "widget", "version": "0.3.0", "transport": {"type": "stdio"}}]
    }"#;

    #[test]
    fn good_cargo_server_json_passes() {
        let r = check_server_json(GOOD);
        assert!(!r.failed(), "{}", r.render("good"));
        assert!(!statuses(&r).contains(&Status::Warn));
    }

    #[test]
    fn init_output_in_a_rust_repo_is_flagged() {
        // What `mcp-publisher init` writes in a repo with no package.json edits.
        let r = check_server_json(
            r#"{"name":"io.github.you/widget","version":"1.0.0",
          "packages":[{"registryType":"npm","identifier":"@YOUR_USERNAME/YOUR_REPO","version":"1.0.0",
          "transport":{"type":"stdio"}}]}"#,
        );
        assert!(
            r.lines
                .iter()
                .any(|(s, m)| *s == Status::Info && m.contains("\"npm\""))
        );
    }

    #[test]
    fn cargo_rules() {
        let r = check_server_json(
            r#"{"$schema":"x","name":"io.github.you/widget","version":"0.3.0",
          "packages":[{"registryType":"cargo","identifier":"widget","fileSha256":"ab",
          "runtimeHint":"cargo","registryBaseUrl":"https://example.com","transport":{"type":"stdio"}}]}"#,
        );
        let text = r.render("t");
        assert!(text.contains("\"version\" is required for cargo"));
        assert!(text.contains("remove \"fileSha256\""));
        assert!(text.contains("registryBaseUrl must be https://crates.io"));
        assert!(text.contains("drop \"runtimeHint\""));
        assert!(r.failed());
    }

    #[test]
    fn version_mismatch_warns() {
        let r = check_server_json(&GOOD.replace(
            "\"version\": \"0.3.0\",\n      \"packages\"",
            "\"version\": \"0.4.0\",\n      \"packages\"",
        ));
        assert!(statuses(&r).contains(&Status::Warn));
        assert!(!r.failed());
    }

    #[test]
    fn untouched_init_output_fails() {
        let r = check_server_json(
            r#"{"name":"com.example/x","description":"An MCP server that provides [describe what your server does]",
          "version":"1.0.0","packages":[{"registryType":"npm","identifier":"@your-org/your-package","version":"1.0.0",
          "transport":{"type":"stdio"},"environmentVariables":[{"name":"YOUR_API_KEY"}]}]}"#,
        );
        assert!(r.failed());
        let text = r.render("t");
        assert!(text.contains("com.example/"));
        assert!(text.contains("no cargo package"));
    }

    #[test]
    fn bad_json_and_bad_name_fail() {
        assert!(check_server_json("{").failed());
        assert!(check_server_json(r#"{"name":"widget","version":"1","packages":[]}"#).failed());
    }
}
