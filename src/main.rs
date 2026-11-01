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
