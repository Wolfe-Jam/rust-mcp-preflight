# Changelog

## 0.1.0 (unreleased)

First release.

- `preflight`: every check on a server.json, from its text.
- `check_server_json`: the registry's rules for cargo packages, plus leftover `mcp-publisher init` placeholders.
- `check_readme`: the `mcp-name:` line in the README crates.io serves for an exact crate version, matched the way the registry matches it.
- `check_name`: the name's pattern, whether it's listed, and which login its namespace needs.
- CLI: `rust-mcp-preflight server.json` runs every check and exits 1 on any failure.
