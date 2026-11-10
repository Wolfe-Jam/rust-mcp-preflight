//! Live integration tests: the built binary against the real services it
//! reads (crates.io, the MCP Registry, the GitHub users API, DNS and HTTPS),
//! through the CLI and over MCP stdio, as a user runs it.
//!
//! They need the network, so they're ignored by default and `cargo test`
//! stays offline. Run them with `cargo test --locked -- --ignored`; the
//! weekly `live` workflow does. Fixtures are chosen not to change: published
//! crate versions are final on crates.io, and `example.com` is reserved
//! (RFC 2606).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_rust-mcp-preflight");

/// A server.json for one cargo package.
fn server_json(name: &str, crate_name: &str, version: &str) -> String {
    json!({
        "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
        "name": name,
        "description": "A live test fixture",
        "version": version,
        "packages": [{
            "registryType": "cargo",
            "registryBaseUrl": "https://crates.io",
            "identifier": crate_name,
            "version": version,
            "transport": { "type": "stdio" }
        }]
    })
    .to_string()
}

/// Runs the CLI on `server_json`; returns its exit code and stdout.
fn cli(server_json: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("preflight-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:x}.json", fnv(server_json)));
    std::fs::write(&path, server_json).unwrap();
    let out = Command::new(BIN).arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

/// Starts the MCP server over stdio, initializes it, sends `request` as id 2
/// and returns that response's `result`.
fn mcp(request: Value) -> Value {
    let mut child = Command::new(BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let send = |stdin: &mut std::process::ChildStdin, v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let wait_for = |id: u64| -> Value {
        loop {
            let line = rx
                .recv_timeout(Duration::from_secs(90))
                .expect("no response from the MCP server");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    };
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "live-test", "version": "0"}}}),
    );
    wait_for(1);
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    let mut req = request;
    req["jsonrpc"] = json!("2.0");
    req["id"] = json!(2);
    send(&mut stdin, req);
    let reply = wait_for(2);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    reply["result"].clone()
}

/// Calls one tool over MCP; returns (isError, text).
fn tool(name: &str, arguments: Value) -> (bool, String) {
    let r = mcp(json!({"method": "tools/call", "params": {"name": name, "arguments": arguments}}));
    (
        r["isError"].as_bool().unwrap_or(false),
        r["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

#[test]
#[ignore = "live: crates.io, the MCP Registry, GitHub"]
fn a_listed_server_with_its_readme_line_is_ready() {
    // 0.2.0 is published, final, and its README carries the line.
    let (code, out) = cli(&server_json(
        "io.github.Wolfe-Jam/rust-mcp-preflight",
        "rust-mcp-preflight",
        "0.2.0",
    ));
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(
            "PASS  README of rust-mcp-preflight 0.2.0 on crates.io contains `mcp-name: io.github.Wolfe-Jam/rust-mcp-preflight`"
        ),
        "{out}"
    );
    assert!(out.contains("INFO  already listed: latest "), "{out}");
    assert!(out.contains("Ready: no failures"), "{out}");
}

#[test]
#[ignore = "live: crates.io"]
fn a_readme_without_the_line_fails() {
    // serde 1.0.0 predates MCP: its README can't have the line.
    let (code, out) = cli(&server_json(
        "io.github.Wolfe-Jam/rust-mcp-preflight",
        "serde",
        "1.0.0",
    ));
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(
            "FAIL  README of serde 1.0.0 doesn't contain `mcp-name: io.github.Wolfe-Jam/rust-mcp-preflight`"
        ),
        "{out}"
    );
}

#[test]
#[ignore = "live: crates.io"]
fn an_unpublished_crate_version_fails() {
    let (code, out) = cli(&server_json(
        "io.github.Wolfe-Jam/rust-mcp-preflight",
        "rust-mcp-preflight",
        "0.0.1",
    ));
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(
            "FAIL  rust-mcp-preflight 0.0.1 not found on crates.io: cargo publish first, then list"
        ),
        "{out}"
    );
}

#[test]
#[ignore = "live: DNS and HTTPS"]
fn a_domain_namespace_without_a_key_fails_with_what_to_add() {
    // example.com is reserved (RFC 2606): no MCPv1 record, and its
    // .well-known URL answers 404.
    let (is_error, text) = tool("check_name", json!({"server_name": "com.example/widget"}));
    assert!(is_error, "{text}");
    assert!(
        text.contains(
            "FAIL  namespace com.example: no published key for `mcp-publisher login dns` or `login http`. Add a TXT record `v=MCPv1; k=ed25519; p=<public key>` at the apex of example.com"
        ),
        "{text}"
    );
}

#[test]
#[ignore = "live: DNS"]
fn a_domain_namespace_with_a_dns_key_passes() {
    // faf.one publishes an Ed25519 MCPv1 TXT record (the maintainer's domain).
    let (is_error, text) = tool(
        "check_name",
        json!({"server_name": "one.faf/claude-faf-mcp"}),
    );
    assert!(!is_error, "{text}");
    assert!(
        text.contains(
            "PASS  DNS: the TXT record at faf.one has a key the registry accepts (ed25519:"
        ),
        "{text}"
    );
    assert!(
        text.contains("`mcp-publisher login dns --domain faf.one` can publish one.faf"),
        "{text}"
    );
}

#[test]
#[ignore = "live: GitHub users API"]
fn an_org_namespace_names_both_ways_to_publish() {
    // Unauthenticated GitHub API calls from shared CI runners can hit the
    // rate limit; both outcomes give the same guidance, so either passes.
    let (_, text) = tool(
        "check_name",
        json!({"server_name": "io.github.modelcontextprotocol/widget"}),
    );
    assert!(
        text.contains("modelcontextprotocol is a GitHub organization.")
            || text
                .contains("couldn't check whether modelcontextprotocol is a GitHub organization"),
        "{text}"
    );
    assert!(text.contains("`mcp-publisher login github-oidc`"), "{text}");
    assert!(
        text.contains("`mcp-publisher login github --token <TOKEN>`"),
        "{text}"
    );
}

#[test]
#[ignore = "live: crates.io, the MCP Registry, GitHub"]
fn the_preflight_tool_matches_the_cli() {
    let text = server_json(
        "io.github.Wolfe-Jam/rust-mcp-preflight",
        "rust-mcp-preflight",
        "0.2.0",
    );
    let (is_error, out) = tool("preflight", json!({"server_json": text}));
    assert!(!is_error, "{out}");
    assert!(out.contains("Ready: no failures"), "{out}");
}

#[test]
fn the_mcp_server_lists_four_read_only_tools() {
    // Offline: tools/list makes no network calls.
    let r = mcp(json!({"method": "tools/list", "params": {}}));
    let tools = r["tools"].as_array().expect("tools");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "check_name",
            "check_readme",
            "check_server_json",
            "preflight"
        ]
    );
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
    }
}
