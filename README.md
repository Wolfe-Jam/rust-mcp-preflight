# Rust MCP Preflight

Check an MCP Registry listing **before** you publish it.

`mcp-publisher validate` checks the shape of `server.json`. It doesn't check that the registry will accept your package: that only happens at `mcp-publisher publish`, after the crate version is already on crates.io and can't be changed. Rust MCP Preflight runs those checks first.

- **The README line.** Fetches the README crates.io serves for the exact crate version and looks for `mcp-name: <your server name>`, matched the way the registry matches it (ported from the registry's own validator). Catches the line hidden in an HTML comment (crates.io strips comments), the line glued to the next word, and a near miss such as the wrong case.
- **server.json.** The rules `mcp-publisher publish` applies: the server name, exact versions (no `latest`, no ranges), and for cargo packages the crate version, no `fileSha256`, `registryBaseUrl` and transport. Plus warnings for `runtimeHint`, mismatched versions, and placeholders left over from `mcp-publisher init`.
- **The server name.** The rules publish applies, whether it's already listed, and which login its namespace needs (including GitHub organizations, which `login github` can't publish to yet).

It only reads: crates.io, the official registry and the GitHub users API. It publishes nothing and needs no keys.

## Install

```sh
cargo install rust-mcp-preflight
```

## Use it from a terminal or CI

```sh
rust-mcp-preflight server.json
```

It prints one line per check and exits 1 if anything fails:

```
Preflight: server.json
  PASS  name "io.github.VoidNullable/lific" fits the pattern namespace/name
  PASS  packages[0]: cargo crate "lific"
  WARN  VoidNullable is a GitHub organization: `mcp-publisher login github` can't publish to org namespaces yet (registry #1468). ...
  INFO  already listed: latest 2.11.1 (active). Your next publish needs a new version number
  PASS  README of lific 2.11.1 on crates.io contains `mcp-name: io.github.VoidNullable/lific`

Ready: no failures, 1 warning(s).
```

## Use it from an MCP client

```sh
claude mcp add preflight -- rust-mcp-preflight
```

```json
{ "mcpServers": { "preflight": { "command": "rust-mcp-preflight" } } }
```

| Tool | What it checks |
|---|---|
| `preflight` | Everything below, from the text of `server.json` |
| `check_server_json` | The publish rules (offline) |
| `check_readme` | The `mcp-name:` line in one crate version's README on crates.io |
| `check_name` | The name's publish rules, listing and namespace login |

## Where it fits

The order is: `cargo publish` (with the line in the README), then `rust-mcp-preflight server.json`, then `mcp-publisher login github` and `mcp-publisher publish`.

The full walk-through: [Listing a Rust MCP server in the official MCP Registry](https://wolfejam.dev/blog/rust-registry-guide).

This version checks cargo packages. Other package types (npm, PyPI, mcpb and so on) are listed as not checked.

If crates.io or the README can't be reached, the README counts as not checked and the result is a failure, so a CI run never passes on a README it didn't see.

Listed and live? Get a free report on the whole server, not just the listing: [wolfejam.dev/#worth-it](https://wolfejam.dev/#worth-it)

## Links

- Repository: https://github.com/Wolfe-Jam/rust-mcp-preflight
- `mcp-name: io.github.Wolfe-Jam/rust-mcp-preflight`

## License

MIT

---

[About wolfejam](https://wolfejam.dev/info)
