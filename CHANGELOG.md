# Changelog

## Unreleased

- Registry drift check: the `Registry drift` workflow (Mondays, and by hand) compares each registry file Preflight ports at the commit it was checked against, `c9fe19b`, with the registry's `main`, and checks that registry issue #1468 (which the org guidance names) is still open. `.github/registry-port.txt` is the one place the pin and the 14 files live: the cargo and `mcp-name` validators, `validators.go`, `schema.go`, `utils.go`, the auth handlers, `jwt.go`, the publisher's GitHub login and `init`, and the authentication docs page. Exit 1 names each file that changed, moved or is listed wrong (with the compare link) and a closed issue; exit 2 means it couldn't check (a malformed list, or GitHub unreachable), never a silent pass. Shown to fail against an older commit (`04623ed`: 10 files changed), on a listed path that doesn't exist, on an empty list and on a closed issue.
- Live integration tests (`tests/live.rs`): the built binary, through the CLI and over MCP stdio, against crates.io, the MCP Registry, the GitHub users API, DNS and HTTPS. Fixtures that don't change: published crate versions (final on crates.io) and `example.com` (reserved, RFC 2606). Ignored by default, so `cargo test` stays offline; `cargo test --locked --test live -- --include-ignored` runs them with the offline one, and the `Live` workflow runs them every Monday and by hand (15-minute limit). When DNS can't be read, the two namespace tests run again up to twice, as Preflight's own message says to. Every test was shown to fail with the behaviour it checks broken (7 mutations: the README matcher both ways, a 404 crate version, DNS keys ignored, a missing key reported as INFO, the org guidance without the OIDC command, a tool not read-only).

## 0.2.0 (2026-11-07)

The namespace login check: for a domain namespace, is the key published where the registry will look, and will it accept it?

- `check_name` (and `preflight`) checks a domain namespace (`com.example/...`) the way `mcp-publisher login dns` and `login http` are verified: a `v=MCPv1; k=<ed25519|ecdsap384>; p=<key>` record as a TXT record at the apex of the domain or a parent domain (DNS covers subdomains), or at `https://<domain>/.well-known/mcp-registry-auth` (that exact domain, public addresses only, `Accept: text/plain`, no redirects, at most 4096 bytes). Keys are parsed as the registry parses them (Ed25519 32 bytes; P-384 49 bytes, compressed, on the curve; Go's whitespace and base64 rules). A record under `_mcp-auth.` or `_mcp-registry.` is named. Each key's fingerprint is printed, to compare when a signature doesn't verify. Ported from the registry's `internal/api/handlers/v0/auth` (`common.go`, `dns.go`, `http.go`).
- A problem with one method fails the check only when no method proves the namespace: a working DNS login next to a redirecting HTTP endpoint is Ready, with the HTTP problem as information. When DNS can't be read (no network, a timeout, SERVFAIL, or an answer with no SOA from a filtering resolver), the check still fails, never "Ready": it says the login wasn't checked, gives every reason, and says to run again.
- New dependencies: `hickory-resolver` (DNS through the system resolver, no third party), `p384` (the P-384 point check), `base64`.
- Checked live on 2026-11-06 against servers published under domain namespaces in the MCP Registry, comparing each with what the registry decides. Two independent reviews ran 19 and 11 more; every result matched. A listed server can lose its key later (`net.agri-ja` now answers HTTP 402), and Preflight then says so.
- The key fetch matches the registry's: it refuses private and loopback addresses, connects only to the addresses it checked (ignoring proxy settings, which would resolve the domain past that check), sends `Accept: text/plain`, and has one 10-second budget with 3 seconds per address. Text from someone else's record is printed escaped and short, and network errors include their cause.

- The GitHub organization warning names the two ways to publish an org server today, with the exact commands: GitHub Actions (`mcp-publisher login github-oidc`) or a token (`mcp-publisher login github --token <TOKEN>`; classic: read:org, fine-grained: Organization > Members > Read-only), and links the registry's authentication page. When GitHub can't be reached, the warning gives the same paths. From the registry's own docs (authentication.mdx) and code (github_oidc.go grants `io.github.<repository_owner>/*`).

## 0.1.3 (2026-11-06)

Setup for more MCP clients, and a README without the sales line.

- README: setup for goose (`--with-extension` and `config.yaml`), VS Code (`code --add-mcp` and `.vscode/mcp.json`) and Cursor (`mcp.json`), each from that client's own documentation, next to Claude Code.
- Tests: the README's client snippets must parse and start `rust-mcp-preflight` over stdio, so a copied config can't drift from the binary.
- README: links the registry validator it ports (`internal/validators/registries/cargo.go`); the registry's validators are unchanged since the port (checked 2026-11-06). The footer is "More about me and my work"; the business card badge moves there.
- LICENSE: copyright holder "James Wolfe".

## 0.1.2 (2026-11-04)

Prebuilt binaries, and the duplicate and over-broad messages from the 0.1.0 review (N5).

- Prebuilt binaries: publishing a GitHub release builds Linux (x86_64, aarch64), macOS (x86_64, aarch64) and Windows (x86_64) binaries and attaches them, each with a SHA-256 file. `cargo binstall rust-mcp-preflight` downloads one in seconds instead of compiling for minutes (`[package.metadata.binstall]`).
- A placeholder inside a longer one that also matched is reported once, as the longer one: `@YOUR_USERNAME/YOUR_REPO` no longer also reports `YOUR_USERNAME`. It still does when `YOUR_USERNAME` also appears on its own.
- `com.example/` is a placeholder only at the start of `name`, so `telecom.example/` elsewhere in the file no longer trips it.
- An invalid name is reported once by `preflight` (it has been since 0.1.0; now a test holds it there).
- README: install with `cargo binstall` first, then `cargo install`, then the release assets.

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
