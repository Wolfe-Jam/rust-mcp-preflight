# Rust MCP Preflight

Check an MCP Registry listing **before** you publish it.

`mcp-publisher validate` checks the shape of `server.json`. It doesn't check that the registry will accept your package: that only happens at `mcp-publisher publish`, after the crate version is already on crates.io and can't be changed. Rust MCP Preflight runs those checks first.

- **The README line.** Fetches the README crates.io serves for the exact crate version and looks for `mcp-name: <your server name>`, matched the way the registry matches it (ported from the registry's own validator, [`internal/validators/registries/cargo.go`](https://github.com/modelcontextprotocol/registry/blob/main/internal/validators/registries/cargo.go); the registry's validators haven't changed since this port, checked 2026-11-06). Catches the line hidden in an HTML comment (crates.io strips comments), the line glued to the next word, and a near miss such as the wrong case.
- **server.json.** The rules `mcp-publisher publish` applies: `$schema`, the server name, and for every package exact versions (no `latest`, no ranges), the transport and no spaces in the identifier. For cargo packages also the crate version, no `fileSha256` and `registryBaseUrl`. Plus warnings for `runtimeHint`, mismatched versions, and placeholders left over from `mcp-publisher init`.
- **The server name.** The rules publish applies, whether it's already listed, and which login its namespace needs (including GitHub organizations, which `login github` can't publish to yet).

It only reads: crates.io, the official registry and the GitHub users API. It publishes nothing and needs no keys.

## Install

Prebuilt, in seconds (needs [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)):

```sh
cargo binstall rust-mcp-preflight
```

From source (compiles, a few minutes the first time):

```sh
cargo install rust-mcp-preflight
```

Or download the binary for your platform from the [release assets](https://github.com/Wolfe-Jam/rust-mcp-preflight/releases/latest): Linux (x86_64, aarch64), macOS (Intel, Apple silicon) and Windows (x86_64), each with a `.sha256` file. Prebuilt binaries start with 0.1.2; for an earlier version, `cargo binstall` falls back to QuickInstall or compiling from source.

## Use it from a terminal or CI

```sh
rust-mcp-preflight server.json
```

It prints one line per check. It exits 0 when ready, 1 if anything fails, and 2 if the file can't be read:

```
Preflight: server.json
  PASS  name "io.github.VoidNullable/lific" is a valid server name
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

A result with a FAIL has `isError` set, like the CLI's exit 1: the check ran, and the listing isn't ready.

## Where it fits

The order is: `cargo publish` (with the line in the README), then `rust-mcp-preflight server.json`, then `mcp-publisher login github` and `mcp-publisher publish`.

The full walk-through: [Listing a Rust MCP server in the official MCP Registry](https://wolfejam.dev/blog/rust-registry-guide).

The README check and the crates.io rules are for cargo packages. Other package types (npm, PyPI, mcpb and so on) get the shared rules, and their registry's own checks are listed as not run.

If crates.io or the README can't be reached, the README counts as not checked and the result is a failure, so a CI run never passes on a README it didn't see.

## Links

- Repository: https://github.com/Wolfe-Jam/rust-mcp-preflight
- `mcp-name: io.github.Wolfe-Jam/rust-mcp-preflight`

## License

MIT

---

More about me and my work: [wolfejam.dev/info](https://wolfejam.dev/info)

[![Business card · Get yours →](https://wolfe-jam.github.io/agent-business-card/badge.svg)](https://wolfe-jam.github.io/agent-business-card/?repo=Wolfe-Jam/rust-mcp-preflight)
