# Changelog

## 0.1.1 (2026-11-01)

Fixes from the 0.1.0 reviews and the first cold install.

- The README is read up to 5 MiB and no further, as the registry's `io.LimitReader` does (0.1.0 downloaded all of it, then cut).
- The name check follows the registry's search pages, so a listed name past the first page is no longer reported as "isn't listed yet". An unreadable answer now warns instead of passing.
- MCP: a result with a FAIL has `isError` set, like the CLI's exit 1.
- `--help` lists the exit codes: 0 ready, 1 any FAIL, 2 the file can't be read.
- CI runs clippy and the tests on Rust 1.88 (the MSRV), not only a build.
- crates.io's Documentation link points to the README: docs.rs builds libraries only, and this crate is a binary.
- README: the install note (the first install compiles for a few minutes), the exit codes, and a business card (`agent.fafa`).

## 0.1.0 (2026-11-01)

First release.

- `preflight`: every check on a server.json, from its text.
- `check_server_json`: the registry's rules for cargo packages, plus leftover `mcp-publisher init` placeholders.
- `check_readme`: the `mcp-name:` line in the README crates.io serves for an exact crate version, matched the way the registry matches it.
- `check_name`: the name's pattern, whether it's listed, and which login its namespace needs.
- CLI: `rust-mcp-preflight server.json` runs every check and exits 1 on any failure.

Before the first publish, two independent reviews (2026-11-01) were checked against the registry's source and fixed:
- A README that couldn't be checked (crates.io 429/5xx, a read error, an inconclusive 403) is now a failure, not a warning: the result is never "Ready" on a README it didn't see.
- A 403 is reported the way the registry's validator does: the version exists but the README couldn't be retrieved, the version isn't on crates.io, or crates.io couldn't be reached.
- Server names follow the rules publish applies (each side at least two characters, starting and ending with a letter or digit), not the looser schema pattern.
- Transports follow the registry: stdio without a url; streamable-http and sse with a valid http(s) url whose {variables} the package declares; other types fail.
- `latest` and version ranges fail; a cargo version that isn't an exact crate version warns.
- An empty `fileSha256` or `registryBaseUrl` is treated as the registry treats it.
- If GitHub's API can't be reached, the organization check says it wasn't checked instead of assuming a personal account.
- Redirects are followed only on the default https port.
- Near-miss `mcp-name:` values are listed only when spaced as the registry's token is.
- `$schema` is required and must be a registry schema URL (as the registry's validateServerJSONSchema).
- Every package type gets the registry's shared rules (version, transport, no spaces in the identifier), not only cargo.
- The version-range patterns match Go's whitespace exactly (no vertical tab).
- An `mcp-name:` with no space or several spaces is named, with the count.
- The crate ships only what it needs (`include`).
