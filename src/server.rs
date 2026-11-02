//! The MCP server: four tools over stdio.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::checks;

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct ServerJsonParams {
    #[schemars(description = "The full text of server.json")]
    pub server_json: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct ReadmeParams {
    #[schemars(description = "Crate name on crates.io (server.json packages[].identifier)")]
    pub crate_name: String,
    #[schemars(description = "Exact crate version to check (server.json packages[].version)")]
    pub version: String,
    #[schemars(description = "MCP server name, e.g. io.github.you/widget")]
    pub server_name: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub struct NameParams {
    #[schemars(description = "MCP server name, e.g. io.github.you/widget")]
    pub server_name: String,
}

#[derive(Debug, Clone)]
pub struct PreflightServer {
    tool_router: ToolRouter<Self>,
}

impl PreflightServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for PreflightServer {
    fn default() -> Self {
        Self::new()
    }
}

/// The report as text. isError is set when it has a FAIL, like the CLI's exit 1:
/// the check ran, and the listing isn't ready.
fn result(report: checks::Report, title: &str) -> CallToolResult {
    let text = vec![ContentBlock::text(report.render(title))];
    if report.failed() {
        CallToolResult::error(text)
    } else {
        CallToolResult::success(text)
    }
}

#[tool_router]
impl PreflightServer {
    #[tool(
        description = "Run every check on a server.json before mcp-publisher publish: its cargo rules, the server name, and the mcp-name line in each cargo package's README on crates.io. Reads only; publishes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn preflight(&self, params: Parameters<ServerJsonParams>) -> CallToolResult {
        result(checks::preflight(&params.0.server_json).await, "Preflight")
    }

    #[tool(
        description = "Check a server.json against the MCP Registry's publish rules: the server name, exact versions (no latest, no ranges), and for cargo packages the crate name and version, no fileSha256, registryBaseUrl and transport. Also warns on runtimeHint, mismatched versions and init placeholders. Offline.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn check_server_json(&self, params: Parameters<ServerJsonParams>) -> CallToolResult {
        result(
            checks::check_server_json(&params.0.server_json),
            "server.json",
        )
    }

    #[tool(
        description = "Check that the README crates.io serves for this exact crate version contains `mcp-name: <server_name>`, matched the way the MCP Registry matches it at publish.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn check_readme(&self, params: Parameters<ReadmeParams>) -> CallToolResult {
        let p = params.0;
        result(
            checks::check_readme(&p.crate_name, &p.version, &p.server_name).await,
            "README on crates.io",
        )
    }

    #[tool(
        description = "Check an MCP server name: the pattern, whether it's already listed in the official registry, and which login its namespace needs.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn check_name(&self, params: Parameters<NameParams>) -> CallToolResult {
        result(
            checks::check_name(&params.0.server_name).await,
            "Server name",
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PreflightServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("rust-mcp-preflight", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Checks an MCP Registry listing before you publish it. Start with `preflight` and the text of server.json. Each line is PASS, FAIL, WARN or INFO; fix every FAIL before `mcp-publisher publish`. A result with a FAIL has isError set: the check ran, and the listing isn't ready."
                .into(),
        );
        info
    }
}
