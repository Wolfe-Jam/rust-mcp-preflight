//! rust-mcp-preflight: check an MCP Registry listing before you publish it.
//!
//! With no arguments it serves MCP over stdio. With a path to server.json it
//! runs the checks once and exits 1 on any failure (for scripts and CI).

use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

mod checks;
mod server;
mod token;

const HELP: &str = "\
rust-mcp-preflight: check an MCP Registry listing before you publish it

Usage:
  rust-mcp-preflight                  Serve MCP over stdio (what MCP clients run)
  rust-mcp-preflight <server.json>    Run every check once; exit 1 on any FAIL
  rust-mcp-preflight --version        Print the version (-V)
  rust-mcp-preflight --help           Print this help (-h)

Exit codes (with a path): 0 ready, 1 any FAIL, 2 the file can't be read.

Run with no arguments, it waits silently for an MCP client. Ctrl+C to quit.

Add it to a client:
  claude mcp add preflight -- rust-mcp-preflight
  { \"mcpServers\": { \"preflight\": { \"command\": \"rust-mcp-preflight\" } } }

https://github.com/Wolfe-Jam/rust-mcp-preflight";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("rust-mcp-preflight {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!("{HELP}");
            return Ok(());
        }
        Some(path) => {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("can't read {path}: {e}");
                    std::process::exit(2);
                }
            };
            let report = checks::preflight(&text).await;
            println!("{}", report.render(&format!("Preflight: {path}")));
            std::process::exit(if report.failed() { 1 } else { 0 });
        }
        None => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let service = server::PreflightServer::new()
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod readme_tests {
    // The README's client setup is copied into config files as it stands, so
    // every JSON snippet must parse and every client must start this binary.
    const README: &str = include_str!("../README.md");

    fn fenced(lang: &str) -> Vec<&'static str> {
        let open = format!("```{lang}\n");
        let mut out = Vec::new();
        let mut rest = README;
        while let Some(i) = rest.find(&open) {
            let body = &rest[i + open.len()..];
            let end = body.find("\n```").expect("unclosed code fence in README");
            out.push(&body[..end]);
            rest = &body[end..];
        }
        out
    }

    fn command_of(server: &serde_json::Value) -> Option<&str> {
        server.get("command").and_then(|c| c.as_str())
    }

    #[test]
    fn readme_json_snippets_parse_and_start_this_binary() {
        let blocks = fenced("json");
        assert!(
            blocks.len() >= 2,
            "expected the VS Code and Cursor snippets"
        );
        for block in blocks {
            let v: serde_json::Value = serde_json::from_str(block)
                .unwrap_or_else(|e| panic!("README JSON doesn't parse ({e}):\n{block}"));
            let servers = v.get("servers").or_else(|| v.get("mcpServers"));
            if let Some(servers) = servers.and_then(|s| s.as_object()) {
                for (name, server) in servers {
                    assert_eq!(command_of(server), Some("rust-mcp-preflight"), "{name}");
                    assert_eq!(
                        server.get("type").and_then(|t| t.as_str()),
                        Some("stdio"),
                        "{name}"
                    );
                }
            }
        }
    }

    #[test]
    fn readme_vs_code_add_mcp_argument_parses() {
        let line = README
            .lines()
            .find(|l| l.starts_with("code --add-mcp '"))
            .expect("README has the code --add-mcp line");
        let arg = line
            .trim_start_matches("code --add-mcp '")
            .trim_end_matches('\'');
        let v: serde_json::Value = serde_json::from_str(arg).expect("--add-mcp JSON parses");
        assert_eq!(v["name"], "preflight");
        assert_eq!(command_of(&v), Some("rust-mcp-preflight"));
    }

    #[test]
    fn readme_cli_clients_run_this_binary() {
        assert!(README.contains("claude mcp add preflight -- rust-mcp-preflight"));
        assert!(README.contains("goose session --with-extension \"rust-mcp-preflight\""));
        let yaml = fenced("yaml").concat();
        assert!(
            yaml.contains("cmd: rust-mcp-preflight"),
            "goose config.yaml snippet"
        );
        assert!(yaml.contains("type: stdio"), "goose config.yaml snippet");
    }
}
