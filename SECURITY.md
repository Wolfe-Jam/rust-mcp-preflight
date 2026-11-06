# Security

Rust MCP Preflight only reads: crates.io, the official MCP Registry and the GitHub users API, over https, and, for a domain namespace, that domain's DNS TXT records (through the system resolver) and `https://<domain>/.well-known/mcp-registry-auth` (no redirects, at most 4096 bytes). It writes nothing, publishes nothing and needs no credentials.

To report a vulnerability, use GitHub's private reporting (Security → Report a vulnerability) on this repository. Please don't open a public issue for it.
