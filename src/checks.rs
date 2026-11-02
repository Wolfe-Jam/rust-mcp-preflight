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

/// An HTTP client that, like the registry's, only follows redirects over https
/// on the default port, to crates.io and static.crates.io (and the registry
/// itself, for the name search).
fn client() -> reqwest::Client {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let ok = attempt.url().scheme() == "https"
            && attempt.url().port().is_none()
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

    // As the registry's validateServerJSONSchema: required, and a URL it can
    // read a schema version from.
    match v.get("$schema").and_then(Value::as_str) {
        None | Some("") => r.push(
            Status::Fail,
            "\"$schema\" is required: add the registry schema URL (https://static.modelcontextprotocol.io/schemas/<version>/server.schema.json)",
        ),
        Some(url) if !schema_url_ok(url) => r.push(
            Status::Fail,
            format!("\"$schema\" {url:?} isn't a registry schema URL (…/schemas/<version>/server.schema.json)"),
        ),
        Some(_) => {}
    }

    let name = v.get("name").and_then(Value::as_str).unwrap_or("");
    match token::server_name_problem(name) {
        None => r.push(
            Status::Pass,
            format!("name \"{name}\" is a valid server name"),
        ),
        Some(why) => r.push(Status::Fail, format!("name: {why}")),
    }

    let top_version = v.get("version").and_then(Value::as_str);
    match top_version {
        None | Some("") => r.push(Status::Fail, "top-level \"version\" is missing"),
        Some(tv) => {
            if let Some(why) = version_problem(tv) {
                r.push(Status::Fail, format!("version: {why}"));
            }
        }
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

        // The registry's validatePackageField, which runs on every package.
        if id.contains(char::is_whitespace) {
            r.push(
                Status::Fail,
                format!("{at}: identifier {id:?} can't contain spaces"),
            );
        }
        if let Some(why) = p
            .get("version")
            .and_then(Value::as_str)
            .and_then(version_problem)
        {
            r.push(Status::Fail, format!("{at}: version: {why}"));
        }
        // As the registry's validatePackageTransport.
        let url = p
            .pointer("/transport/url")
            .and_then(Value::as_str)
            .unwrap_or("");
        match p.pointer("/transport/type").and_then(Value::as_str) {
            Some("stdio") if !url.is_empty() => r.push(
                Status::Fail,
                format!("{at}: transport stdio must not have a url (got {url})"),
            ),
            Some("stdio") => {}
            Some(t @ ("streamable-http" | "sse")) if url.is_empty() => {
                r.push(Status::Fail, format!("{at}: transport {t} needs a url"))
            }
            Some(t @ ("streamable-http" | "sse")) => {
                if is_valid_templated_url(url, &package_variables(p)) {
                    r.push(Status::Info, format!("{at}: transport {t}"));
                } else {
                    r.push(
                        Status::Fail,
                        format!("{at}: transport url {url:?} isn't a valid http(s) url, or uses a {{variable}} the package doesn't declare"),
                    );
                }
            }
            Some(t) => r.push(
                Status::Fail,
                format!("{at}: unsupported transport type \"{t}\" (stdio, streamable-http or sse)"),
            ),
            None => r.push(
                Status::Fail,
                format!("{at}: \"transport\" is missing (usually {{\"type\": \"stdio\"}})"),
            ),
        }

        if kind != "cargo" {
            r.push(
                Status::Info,
                format!("{at}: the {kind:?} registry's own checks aren't run by this version (cargo only)"),
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
                if version_problem(pv).is_none() && !is_exact_semver(pv) {
                    r.push(
                        Status::Warn,
                        format!("{at}: version \"{pv}\" isn't an exact crate version (like 0.3.0): crates.io can't look it up"),
                    );
                }
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
        match p.get("fileSha256") {
            Some(Value::String(h)) if !h.is_empty() => r.push(
                Status::Fail,
                format!("{at}: remove \"fileSha256\": the registry rejects it on cargo packages (it's for mcpb only)"),
            ),
            Some(Value::String(_)) | Some(Value::Null) => r.push(
                Status::Warn,
                format!("{at}: remove the empty \"fileSha256\": it's for mcpb only"),
            ),
            Some(_) => r.push(
                Status::Fail,
                format!("{at}: remove \"fileSha256\": it must be a string, and it's for mcpb only"),
            ),
            None => {}
        }
        // Empty means the default, https://crates.io (as the registry does).
        if let Some(base) = p.get("registryBaseUrl").and_then(Value::as_str)
            && !base.is_empty()
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
            r.push(Status::Fail, format!("README not checked: crates.io returned {status} for the readme metadata. Likely temporary: run again before you publish"));
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

    let mut resp = match http
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
        // The four outcomes of the registry's cargoReadme403Error.
        match exists {
            Ok(200) => r.push(
                Status::Fail,
                format!("{crate_name} {version} exists on crates.io, but its rendered README couldn't be retrieved (403). If it has no README, add one containing `mcp-name: {server_name}` and publish a new version"),
            ),
            Ok(404) => r.push(
                Status::Fail,
                format!("{crate_name} {version} not found on crates.io: cargo publish first, then list"),
            ),
            Ok(s) if transient(s) => r.push(
                Status::Fail,
                format!("README not checked: crates.io couldn't confirm {crate_name} {version} ({s}). Likely temporary: run again before you publish"),
            ),
            Err(_) => r.push(
                Status::Fail,
                format!("README not checked: couldn't reach crates.io to confirm {crate_name} {version}. Run again before you publish"),
            ),
            Ok(s) => r.push(
                Status::Fail,
                format!("{crate_name} {version} not found on crates.io (README 403, version lookup {s})"),
            ),
        }
        return r;
    }
    if status != 200 {
        let msg = if transient(status) {
            format!(
                "README not checked: the rendered README fetch returned {status}. Likely temporary: run again before you publish"
            )
        } else {
            format!("rendered README fetch failed ({status})")
        };
        r.push(Status::Fail, msg);
        return r;
    }
    // Read at most 5 MiB, like the registry's io.LimitReader; the rest is never downloaded.
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if append_capped(&mut bytes, &chunk, MAX_README_BYTES) {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                r.push(
                    Status::Fail,
                    format!("README not checked: failed to read the rendered README ({e}). Run again before you publish"),
                );
                return r;
            }
        }
    }
    // Invalid UTF-8 becomes U+FFFD, a non-name character, so token boundaries are unchanged.
    let body = String::from_utf8_lossy(&bytes);

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
    for (value, spaces) in token::spacing_misses(&body) {
        r.push(
            Status::Info,
            format!("found `mcp-name:` followed by {spaces} spaces before {value}; the registry needs exactly one space"),
        );
    }
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
    if let Some(why) = token::server_name_problem(server_name) {
        r.push(Status::Fail, format!("name: {why}"));
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
                .and_then(|v| v["type"].as_str().map(String::from))
                .or(Some("unchecked no account type in the reply".to_string())),
            Ok(resp) => Some(format!("unchecked {}", resp.status().as_u16())),
            Err(_) => Some("unchecked no connection".to_string()),
        };
        match kind.as_deref() {
            Some("missing") => r.push(Status::Fail, format!("no GitHub account or organization named {owner}")),
            Some("Organization") => r.push(
                Status::Warn,
                format!("{owner} is a GitHub organization: `mcp-publisher login github` can't publish to org namespaces yet (registry #1468). Be an org Owner, then publish from GitHub Actions or log in with a token that has read:org"),
            ),
            Some(k) if k.starts_with("unchecked") => r.push(
                Status::Warn,
                format!("couldn't check whether {owner} is a GitHub organization (GitHub API: {}). If it is, `mcp-publisher login github` can't publish there yet (registry #1468)", k.trim_start_matches("unchecked ")),
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

    match find_listing(&http, server_name).await {
        Ok(Some(s)) => {
            let ver = s["server"]["version"].as_str().unwrap_or("?");
            let st = s["_meta"]["io.modelcontextprotocol.registry/official"]["status"]
                .as_str()
                .unwrap_or("?");
            r.push(
                Status::Info,
                format!("already listed: latest {ver} ({st}). Your next publish needs a new version number"),
            );
        }
        Ok(None) => r.push(
            Status::Pass,
            format!("{server_name} isn't listed yet: first publish"),
        ),
        Err(why) => r.push(
            Status::Warn,
            format!("couldn't check whether it's listed: {why}"),
        ),
    }
    r
}

/// Adds `chunk` to `buf` without going past `cap` bytes. True once `buf` is full.
fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    let room = cap.saturating_sub(buf.len());
    buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
    buf.len() >= cap
}

/// What one page of registry search results says about an exact name.
#[derive(Debug, PartialEq)]
enum Page {
    /// The listing with exactly this name.
    Hit(Value),
    /// Not on this page; the cursor for the next one.
    Next(String),
    /// Not on this page, and there are no more.
    End,
    /// No `servers` list: an answer that can't say either way.
    Unreadable,
}

fn scan_page(v: &Value, server_name: &str) -> Page {
    let Some(servers) = v["servers"].as_array() else {
        return Page::Unreadable;
    };
    if let Some(hit) = servers
        .iter()
        .find(|s| s["server"]["name"].as_str() == Some(server_name))
    {
        return Page::Hit(hit.clone());
    }
    match v["metadata"]["nextCursor"].as_str() {
        Some(c) if !c.is_empty() => Page::Next(c.to_string()),
        _ => Page::End,
    }
}

/// Most search pages fetched before giving up (100 results each).
const SEARCH_MAX_PAGES: usize = 20;

/// The latest listing with exactly this name. Search matches substrings, so
/// io.github.you/widget also returns widget-pro: follow nextCursor until the
/// exact name turns up or the results end.
async fn find_listing(http: &reqwest::Client, server_name: &str) -> Result<Option<Value>, String> {
    let mut cursor: Option<String> = None;
    for _ in 0..SEARCH_MAX_PAGES {
        let mut url = format!(
            "{REGISTRY}/v0.1/servers?search={}&version=latest&limit=100",
            urlencode(server_name)
        );
        if let Some(c) = &cursor {
            url.push_str(&format!("&cursor={}", urlencode(c)));
        }
        let resp = http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("couldn't reach the registry ({e})"))?;
        if !resp.status().is_success() {
            return Err(format!("registry search returned {}", resp.status()));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("couldn't read the registry's answer ({e})"))?;
        match scan_page(&v, server_name) {
            Page::Hit(hit) => return Ok(Some(hit)),
            Page::Next(c) => cursor = Some(c),
            Page::End => return Ok(None),
            Page::Unreadable => {
                return Err("the registry's answer had no servers list".into());
            }
        }
    }
    Err(format!(
        "the search for it went past {} results",
        SEARCH_MAX_PAGES * 100
    ))
}

// ─── All of it ──────────────────────────────────────────────────────────

/// server.json, then the name, then each cargo package's README.
pub async fn preflight(server_json: &str) -> Report {
    let mut r = check_server_json(server_json);
    let Ok(v) = serde_json::from_str::<Value>(server_json) else {
        return r;
    };
    let name = v["name"].as_str().unwrap_or("").to_string();
    // A bad name is already reported above, and nothing below can pass without it.
    if token::server_name_problem(&name).is_some() {
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

/// Why the registry would refuse this version string, or `None`.
/// Mirrors validateVersion: "latest" is reserved, and ranges are refused.
fn version_problem(v: &str) -> Option<String> {
    use std::sync::LazyLock;
    // The registry's patterns (validators.go), unchanged.
    static RANGES: LazyLock<[regex::Regex; 3]> = LazyLock::new(|| {
        [
            regex::Regex::new(r"(?-u)^[\t\n\f\r ]*(?:\^|~|>=|<=|>|<|=)[\t\n\f\r ]*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]*$").unwrap(),
            regex::Regex::new(r"(?-u)^[\t\n\f\r ]*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]-[\t\n\f\r ]*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]*$").unwrap(),
            regex::Regex::new(r"(?-u)^[\t\n\f\r ]*(?:v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]*)(?:\|\|[\t\n\f\r ]*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]*)+$").unwrap(),
        ]
    });
    static DOTTED: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?-u)^[\t\n\f\r ]*(?:v?\d+|x|X|\*)(?:\.(?:\d+|x|X|\*)){1,2}(?:-[0-9A-Za-z.-]+)?[\t\n\f\r ]*$",
        )
        .unwrap()
    });
    if v == "latest" {
        return Some("\"latest\" is reserved: use the exact version".into());
    }
    let t = v.trim();
    if t.is_empty() {
        return None;
    }
    let range = RANGES.iter().any(|re| re.is_match(t))
        || (DOTTED.is_match(t) && (t.contains('x') || t.contains('X') || t.contains('*')));
    range.then(|| format!("\"{v}\" looks like a range: use one exact version"))
}

/// The variable names a package declares, which a transport url may use as
/// `{name}`. Mirrors the registry's collectAvailableVariables.
fn package_variables(p: &Value) -> Vec<String> {
    let mut vars = Vec::new();
    for env in p["environmentVariables"].as_array().into_iter().flatten() {
        if let Some(n) = env["name"].as_str() {
            vars.push(n.to_string());
        }
    }
    for key in ["runtimeArguments", "packageArguments"] {
        for arg in p[key].as_array().into_iter().flatten() {
            for field in ["name", "valueHint"] {
                if let Some(v) = arg[field].as_str().filter(|v| !v.is_empty()) {
                    vars.push(v.to_string());
                }
            }
        }
    }
    vars
}

/// Mirrors the registry's IsValidTemplatedURL: after template variables are
/// replaced with stand-ins, an http(s) url with a host; and every `{variable}`
/// is one the package declares.
fn is_valid_templated_url(raw: &str, available: &[String]) -> bool {
    use std::sync::LazyLock;
    static PORT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r":(\{[^}]+\})(/|$)").unwrap());
    static VAR: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\{([^}]+)\}").unwrap());
    let mut test = raw.to_string();
    for (k, v) in [
        ("{host}", "example.com"),
        ("{port}", "8080"),
        ("{path}", "api"),
        ("{protocol}", "http"),
        ("{scheme}", "http"),
    ] {
        test = test.replace(k, v);
    }
    let test = PORT.replace_all(&test, ":8080$2");
    let test = VAR.replace_all(&test, "placeholder");
    let Ok(u) = url::Url::parse(&test) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none_or(str::is_empty) {
        return false;
    }
    VAR.captures_iter(raw)
        .all(|c| available.iter().any(|a| a == &c[1]))
}

/// The registry's extractVersionFromSchemaURL: the URL must contain
/// `/schemas/<version>/server.schema.json`.
fn schema_url_ok(url: &str) -> bool {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?-u)/schemas/([A-Za-z0-9_~.-]+)/server\.schema\.json").unwrap()
    });
    RE.is_match(url)
}

/// An exact crate version, as crates.io stores it: MAJOR.MINOR.PATCH, with an
/// optional pre-release and build.
fn is_exact_semver(v: &str) -> bool {
    use std::sync::LazyLock;
    static SEMVER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?-u)^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$").unwrap()
    });
    SEMVER.is_match(v)
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
            r#"{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.github.you/widget","version":"0.3.0",
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

    fn with_pkg(extra: &str) -> String {
        format!(
            r#"{{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.github.you/widget","version":"0.3.0",
              "packages":[{{"registryType":"cargo","identifier":"widget","version":"0.3.0",{extra}}}]}}"#
        )
    }

    #[test]
    fn transport_rules_match_the_registry() {
        let fails = |extra: &str| check_server_json(&with_pkg(extra)).failed();
        assert!(!fails(r#""transport":{"type":"stdio"}"#));
        assert!(fails(
            r#""transport":{"type":"stdio","url":"http://example.com"}"#
        ));
        assert!(fails(r#""transport":{"type":"streamable-http"}"#));
        assert!(!fails(
            r#""transport":{"type":"streamable-http","url":"https://example.com/mcp"}"#
        ));
        assert!(fails(r#""transport":{"type":"http"}"#));
        assert!(fails(r#""other":1"#));
    }

    #[test]
    fn transport_urls_like_the_registry() {
        let fails = |extra: &str| check_server_json(&with_pkg(extra)).failed();
        assert!(fails(
            r#""transport":{"type":"streamable-http","url":"not a url"}"#
        ));
        assert!(fails(
            r#""transport":{"type":"sse","url":"ftp://example.com"}"#
        ));
        // {host} and {port} get stand-ins for the shape check, but like any
        // {variable} they must still be declared by the package.
        assert!(fails(
            r#""transport":{"type":"streamable-http","url":"http://{host}:{port}/mcp"}"#
        ));
        assert!(!fails(
            r#""transport":{"type":"streamable-http","url":"http://{host}:{port}/mcp"},"environmentVariables":[{"name":"host"},{"name":"port"}]"#
        ));
        assert!(fails(
            r#""transport":{"type":"streamable-http","url":"https://example.com/{token}"}"#
        ));
        assert!(!fails(
            r#""transport":{"type":"streamable-http","url":"https://example.com/{TOKEN}"},"environmentVariables":[{"name":"TOKEN"}]"#
        ));
    }

    #[test]
    fn schema_is_required_and_must_be_a_registry_url() {
        assert!(
            check_server_json(&GOOD.replace(
                "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
                ""
            ))
            .failed()
        );
        assert!(
            check_server_json(&GOOD.replace(
                "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
                "x"
            ))
            .failed()
        );
        let no_schema = r#"{"name":"io.github.you/widget","version":"0.3.0","packages":[{"registryType":"cargo","identifier":"widget","version":"0.3.0","transport":{"type":"stdio"}}]}"#;
        assert!(check_server_json(no_schema).failed());
    }

    #[test]
    fn vertical_tab_is_not_whitespace_as_in_go() {
        // Go RE2 \s has no \x0B; publish accepts this, so we must too.
        assert_eq!(version_problem("1.0.0\u{000b}-\u{000b}2.0.0"), None);
        assert!(version_problem("1.0.0 - 2.0.0").is_some());
    }

    #[test]
    fn every_package_gets_the_shared_rules() {
        let npm = |extra: &str| {
            format!(
                r#"{{"$schema":"https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json","name":"io.github.you/widget","version":"1.0.0",
                  "packages":[{{"registryType":"npm","identifier":"widget","version":"1.0.0",{extra}}}]}}"#
            )
        };
        assert!(!check_server_json(&npm(r#""transport":{"type":"stdio"}"#)).failed());
        assert!(check_server_json(&npm(r#""transport":{"type":"http"}"#)).failed());
        assert!(
            check_server_json(&npm(r#""transport":{"type":"stdio"}"#).replace(
                "\"version\":\"1.0.0\",\"transport",
                "\"version\":\"^1.0.0\",\"transport"
            ))
            .failed()
        );
        assert!(
            check_server_json(
                &npm(r#""transport":{"type":"stdio"}"#)
                    .replace("\"identifier\":\"widget\"", "\"identifier\":\"my widget\"")
            )
            .failed()
        );
    }

    #[test]
    fn non_string_sha_fails() {
        assert!(
            check_server_json(&with_pkg(r#""transport":{"type":"stdio"},"fileSha256":1"#)).failed()
        );
    }

    #[test]
    fn latest_and_ranges_fail() {
        for v in [
            "latest",
            "^1.2.3",
            "~1.2",
            ">=1.0.0",
            "1.2.x",
            "1.2 || 1.3",
            "1.0.0 - 2.0.0",
        ] {
            assert!(version_problem(v).is_some(), "{v}");
        }
        for v in ["0.3.0", "1.0.0-beta.1", "1", "2.0"] {
            assert_eq!(version_problem(v), None, "{v}");
        }
        let json = GOOD.replace("\"0.3.0\"", "\"latest\"");
        assert!(check_server_json(&json).failed());
    }

    #[test]
    fn inexact_crate_version_warns() {
        let r = check_server_json(&GOOD.replace("\"0.3.0\"", "\"1\""));
        assert!(!r.failed());
        assert!(r.render("t").contains("isn't an exact crate version"));
    }

    #[test]
    fn empty_sha_and_base_url_like_the_registry() {
        let r = check_server_json(&with_pkg(
            r#""transport":{"type":"stdio"},"fileSha256":"","registryBaseUrl":"""#,
        ));
        assert!(!r.failed(), "{}", r.render("t"));
        assert!(statuses(&r).contains(&Status::Warn));
    }

    #[test]
    fn short_name_fails() {
        let r = check_server_json(&GOOD.replace("io.github.you/widget", "a/b"));
        assert!(r.failed());
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

    #[test]
    fn append_capped_stops_at_the_cap() {
        let mut buf = Vec::new();
        assert!(!append_capped(&mut buf, b"abc", 5));
        assert!(append_capped(&mut buf, b"defgh", 5));
        assert_eq!(buf, b"abcde");
        // Full: more input adds nothing.
        assert!(append_capped(&mut buf, b"xyz", 5));
        assert_eq!(buf.len(), 5);
    }

    #[test]
    fn readme_cap_drops_the_byte_past_5_mib() {
        let mut buf = Vec::new();
        let big = vec![b'a'; MAX_README_BYTES + 1];
        assert!(append_capped(&mut buf, &big, MAX_README_BYTES));
        assert_eq!(buf.len(), MAX_README_BYTES);
    }

    #[test]
    fn character_cut_at_the_cap_ends_the_name() {
        // The token, then a 2-byte character split by the cap.
        let name = "io.github.you/widget";
        let input = format!("x x mcp-name: {name}\u{e9} tail");
        let cap = input.find('\u{e9}').unwrap() + 1;
        let mut buf = Vec::new();
        assert!(append_capped(&mut buf, input.as_bytes(), cap));
        let body = String::from_utf8_lossy(&buf);
        assert!(body.ends_with('\u{FFFD}'));
        assert!(token::contains_token(&body, name));
    }

    fn page(names: &[&str], next: Option<&str>) -> Value {
        let servers: Vec<Value> = names
            .iter()
            .map(|n| serde_json::json!({ "server": { "name": n, "version": "1.0.0" } }))
            .collect();
        let mut v = serde_json::json!({ "servers": servers, "metadata": { "count": names.len() } });
        if let Some(c) = next {
            v["metadata"]["nextCursor"] = Value::from(c);
        }
        v
    }

    #[test]
    fn scan_page_exact_hit() {
        let v = page(
            &["io.github.you/widget", "io.github.you/widget-pro"],
            Some("c"),
        );
        match scan_page(&v, "io.github.you/widget") {
            Page::Hit(h) => assert_eq!(h["server"]["name"], "io.github.you/widget"),
            other => panic!("expected a hit, got {other:?}"),
        }
    }

    #[test]
    fn scan_page_substring_only_goes_to_the_next_page() {
        let v = page(
            &["io.github.you/widget-pro"],
            Some("io.github.you/widget-pro:1.0.0"),
        );
        assert_eq!(
            scan_page(&v, "io.github.you/widget"),
            Page::Next("io.github.you/widget-pro:1.0.0".into())
        );
    }

    #[test]
    fn scan_page_ends_on_empty_or_missing_cursor() {
        assert_eq!(
            scan_page(&page(&["a.b/other"], Some("")), "a.b/widget"),
            Page::End
        );
        assert_eq!(
            scan_page(&page(&["a.b/other"], None), "a.b/widget"),
            Page::End
        );
        assert_eq!(scan_page(&page(&[], None), "a.b/widget"), Page::End);
    }

    #[test]
    fn scan_page_without_servers_is_unreadable_not_unlisted() {
        assert_eq!(
            scan_page(&serde_json::json!({}), "a.b/widget"),
            Page::Unreadable
        );
        let no_list = serde_json::json!({ "servers": null, "metadata": {} });
        assert_eq!(scan_page(&no_list, "a.b/widget"), Page::Unreadable);
    }
}
