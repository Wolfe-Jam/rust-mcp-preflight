//! The namespace login check for domain namespaces (`com.example/...`): is a
//! public key published where `mcp-publisher login dns` or `login http` will
//! look for it, and will the registry accept it?
//!
//! Ported from the registry's auth handlers
//! (`internal/api/handlers/v0/auth/`): `common.go` (record pattern, key
//! parsing, permissions), `dns.go` (apex TXT lookup, wrong-selector probe) and
//! `http.go` (`/.well-known/mcp-registry-auth`, no redirects, 4096-byte cap).
//!
//! It reads the published key only. It can't check that your private key
//! matches it, so it prints each key's fingerprint the way the registry does
//! when a signature doesn't verify.

use std::time::Duration;

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use regex::Regex;

use crate::checks::{Report, Status, USER_AGENT};

/// Go's `base64.StdEncoding`: padded, and (not being `.Strict()`) it accepts
/// non-zero trailing bits.
const STD_ENCODING: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

/// `MCPProofRecordPattern` in common.go. Go's `\s` is `[\t\n\f\r ]`, not
/// Unicode whitespace, so it's spelled out.
fn record_pattern() -> Regex {
    Regex::new(r"v=MCPv1;[\t\n\x0C\r ]*k=([^;]+);[\t\n\x0C\r ]*p=([A-Za-z0-9+/=]+)")
        .expect("valid pattern")
}

/// `commonWrongSelectors` in dns.go: where people put the record by mistake.
const WRONG_SELECTORS: [&str; 2] = ["_mcp-auth", "_mcp-registry"];

/// `MaxKeyResponseSize` in http.go.
const MAX_KEY_RESPONSE: usize = 4096;

/// A key the registry would accept, shown as its fingerprint (`ed25519:AbCdEfGh`).
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub fingerprint: String,
}

/// `ParsePublicKey` in common.go.
pub fn parse_key(algorithm: &str, public_key: &str) -> Result<Key, String> {
    let bytes = STD_ENCODING
        .decode(public_key)
        .map_err(|e| format!("failed to decode public key: {e}"))?;
    match algorithm {
        "ed25519" => {
            if bytes.len() != 32 {
                return Err("invalid Ed25519 public key size".into());
            }
        }
        "ecdsap384" => {
            if bytes.len() != 49 {
                return Err("invalid ECDSA P-384 public key size".into());
            }
            if bytes[0] != 0x02 && bytes[0] != 0x03 {
                return Err("invalid ECDSA P-384 public key format (must be compressed, with a leading 0x02 or 0x03 byte)".into());
            }
            if p384::PublicKey::from_sec1_bytes(&bytes).is_err() {
                return Err("failed to decompress ECDSA P-384 public key".into());
            }
        }
        other => return Err(format!("unsupported public key algorithm: {other}")),
    }
    // Fingerprint in common.go: the algorithm and the first 8 base64 characters.
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(Key {
        fingerprint: format!("{algorithm}:{}", &encoded[..encoded.len().min(8)]),
    })
}

/// What a set of records (DNS TXT strings, or the one HTTP body) proves.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// At least one key the registry would accept.
    Keys(Vec<Key>),
    /// MCPv1 records, but none the registry would accept: why.
    Invalid(String),
    /// No MCPv1 record at all.
    None,
}

/// `ParseMCPKeysFromStrings` plus the checks in `ExchangeToken` (common.go):
/// no record → none; a single record that fails → its error; otherwise the
/// valid keys, or "no valid MCP public key found".
pub fn evaluate(records: &[String]) -> Found {
    let pattern = record_pattern();
    let parsed: Vec<Result<Key, String>> = records
        .iter()
        .filter_map(|r| pattern.captures(r))
        .map(|c| parse_key(&c[1], &c[2]))
        .collect();
    if parsed.is_empty() {
        return Found::None;
    }
    if parsed.len() == 1
        && let Err(e) = &parsed[0]
    {
        return Found::Invalid(e.clone());
    }
    let keys: Vec<Key> = parsed.into_iter().filter_map(Result::ok).collect();
    if keys.is_empty() {
        Found::Invalid("no valid MCP public key found".into())
    } else {
        Found::Keys(keys)
    }
}

/// `IsValidDomain` in common.go.
fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 || domain.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    Regex::new(
        r"^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*$",
    )
    .expect("valid pattern")
    .is_match(domain)
}

/// The domain a namespace names: `com.example.api` → `api.example.com`
/// (`ReverseString` in common.go).
pub fn domain_of(namespace: &str) -> String {
    namespace.split('.').rev().collect::<Vec<_>>().join(".")
}

/// The domains whose DNS login can publish `namespace`, closest first. A DNS
/// login for D grants `reverse(D)/*` and `reverse(D).*` (`BuildPermissions`
/// with subdomains), so `com.example.api` is covered by `api.example.com` and
/// by `example.com`, but not by `com` or by `example-other.com`.
pub fn dns_domains(namespace: &str) -> Vec<String> {
    let labels: Vec<&str> = namespace.split('.').collect();
    (2..=labels.len())
        .rev()
        .map(|n| domain_of(&labels[..n].join(".")))
        .filter(|d| is_valid_domain(d))
        .collect()
}

fn is_github_pages(domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    d == "github.io" || d.ends_with(".github.io")
}

fn fingerprints(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| k.fingerprint.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A TXT lookup: the record strings (each record's strings joined, as Go's
/// `LookupTXT` returns them), `Ok(empty)` when the name has no TXT records,
/// or why the lookup failed.
async fn txt(
    resolver: &TokioResolver,
    name: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let lookup = tokio::time::timeout(timeout, resolver.txt_lookup(format!("{name}.")))
        .await
        .map_err(|_| format!("DNS lookup of {name} timed out"))?;
    match lookup {
        Ok(found) => Ok(found
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::TXT(t) => Some(
                    t.txt_data
                        .iter()
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()),
        Err(e) if e.is_no_records_found() || e.is_nx_domain() => Ok(Vec::new()),
        Err(e) => Err(format!("DNS lookup of {name} failed: {e}")),
    }
}

/// The HTTP key, read as http.go reads it: no redirects, 200 only, at most
/// 4096 bytes, trimmed. `Ok(None)` for a 404 (nothing published).
async fn http_key(domain: &str) -> Result<Option<String>, String> {
    let url = format!("https://{domain}/.well-known/mcp-registry-auth");
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if resp.status().is_redirection() {
        return Err(format!(
            "{url} answers HTTP {status} (a redirect); the registry doesn't follow redirects, so serve the key at this URL"
        ));
    }
    if status != 200 {
        return Err(format!(
            "{url} answers HTTP {status}; the registry needs 200"
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{url}: {e}"))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_KEY_RESPONSE {
            return Err(format!(
                "{url} is over {MAX_KEY_RESPONSE} bytes; the registry reads no more than that"
            ));
        }
    }
    Ok(Some(String::from_utf8_lossy(&body).trim().to_string()))
}

const AUTH_DOCS: &str = "https://modelcontextprotocol.io/registry/authentication";

/// The namespace login check for a domain namespace, appended to `r`.
pub async fn check_namespace_login(namespace: &str, r: &mut Report) {
    let domain = domain_of(namespace);
    if is_github_pages(&domain) {
        r.push(
            Status::Fail,
            format!("namespace {namespace}: github.io domains can't use DNS or HTTP login; use the io.github.<you> namespace with `mcp-publisher login github`"),
        );
        return;
    }
    let candidates = dns_domains(namespace);
    if candidates.is_empty() {
        r.push(
            Status::Fail,
            format!("namespace {namespace}: {domain} isn't a domain the registry can verify"),
        );
        return;
    }

    let mut proven = false;
    let mut unchecked: Vec<String> = Vec::new();
    // A problem with one method matters only if no method proves the namespace.
    let mut problems: Vec<String> = Vec::new();

    // DNS, closest domain first.
    match TokioResolver::builder_tokio() {
        Err(e) => unchecked.push(format!("no DNS resolver: {e}")),
        Ok(builder) => {
            let resolver = builder.build().map_err(|e| e.to_string());
            match resolver {
                Err(e) => unchecked.push(format!("no DNS resolver: {e}")),
                Ok(resolver) => {
                    for d in &candidates {
                        match txt(&resolver, d, Duration::from_secs(5)).await {
                            Err(why) => unchecked.push(why),
                            Ok(records) => match evaluate(&records) {
                                Found::Keys(keys) => {
                                    r.push(
                                        Status::Pass,
                                        format!(
                                            "DNS: the TXT record at {d} has a key the registry accepts ({}): `mcp-publisher login dns --domain {d}` can publish {namespace}",
                                            fingerprints(&keys)
                                        ),
                                    );
                                    proven = true;
                                    break;
                                }
                                Found::Invalid(why) => problems.push(format!(
                                    "DNS: the MCPv1 TXT record at {d} won't verify: {why}"
                                )),
                                Found::None => {}
                            },
                        }
                    }
                    if !proven {
                        for d in &candidates {
                            for sel in WRONG_SELECTORS {
                                let name = format!("{sel}.{d}");
                                if let Ok(records) =
                                    txt(&resolver, &name, Duration::from_secs(2)).await
                                    && evaluate(&records) != Found::None
                                {
                                    problems.push(format!("DNS: an MCPv1 record is at {name}, but the registry reads only the apex, {d}: move it there"));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // HTTP: only the exact domain (no subdomain grant).
    match http_key(&domain).await {
        Err(why) => problems.push(format!("HTTP: {why}")),
        Ok(None) => {}
        Ok(Some(body)) => match evaluate(&[body]) {
            Found::Keys(keys) => {
                r.push(
                    Status::Pass,
                    format!(
                        "HTTP: https://{domain}/.well-known/mcp-registry-auth has a key the registry accepts ({}): `mcp-publisher login http --domain {domain}` can publish {namespace}",
                        fingerprints(&keys)
                    ),
                );
                proven = true;
            }
            Found::Invalid(why) => problems.push(format!(
                "HTTP: the key at https://{domain}/.well-known/mcp-registry-auth won't verify: {why}"
            )),
            Found::None => problems.push(format!(
                "HTTP: https://{domain}/.well-known/mcp-registry-auth has no `v=MCPv1; k=...; p=...` record"
            )),
        },
    }

    for (status, msg) in verdict(
        namespace,
        &domain,
        &candidates[0],
        proven,
        problems,
        unchecked,
    ) {
        r.push(status, msg);
    }
}

/// The namespace login's verdict. A problem with one method (DNS or HTTP)
/// fails the check only when no method proves the namespace: once one does,
/// the other's problems are information.
fn verdict(
    namespace: &str,
    domain: &str,
    apex: &str,
    proven: bool,
    problems: Vec<String>,
    unchecked: Vec<String>,
) -> Vec<(Status, String)> {
    if proven {
        let mut out: Vec<(Status, String)> = problems
            .into_iter()
            .map(|p| (Status::Info, format!("not needed for this login: {p}")))
            .collect();
        out.push((
            Status::Info,
            "Preflight checks the published key, not your private key: if publish says the signature doesn't verify, compare the fingerprint above with your key's".to_string(),
        ));
        out
    } else if !problems.is_empty() {
        problems.into_iter().map(|p| (Status::Fail, p)).collect()
    } else if !unchecked.is_empty() {
        vec![(
            Status::Fail,
            format!(
                "namespace {namespace}: couldn't confirm a published key ({}), so the login isn't checked",
                unchecked.join("; ")
            ),
        )]
    } else {
        vec![(
            Status::Fail,
            format!(
                "namespace {namespace}: no published key for `mcp-publisher login dns` or `login http`. Add a TXT record `v=MCPv1; k=ed25519; p=<public key>` at the apex of {apex} (DNS also works from a parent domain), or serve it at https://{domain}/.well-known/mcp-registry-auth. {AUTH_DOCS}"
            ),
        )]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_methods_problem_fails_only_when_nothing_proves_the_namespace() {
        let http_redirect =
            || vec!["HTTP: https://x.example/... answers HTTP 301 (a redirect)".to_string()];
        // DNS proved it: the HTTP redirect is information, not a failure
        let v = verdict(
            "com.example",
            "example.com",
            "example.com",
            true,
            http_redirect(),
            vec![],
        );
        assert!(v.iter().all(|(s, _)| *s == Status::Info), "{v:?}");
        assert!(v[0].1.starts_with("not needed for this login: HTTP:"));
        // nothing proved it: the same problem fails
        let v = verdict(
            "com.example",
            "example.com",
            "example.com",
            false,
            http_redirect(),
            vec![],
        );
        assert_eq!(v, vec![(Status::Fail, http_redirect()[0].clone())]);
        // nothing found, nothing checked → the unchecked reason fails
        let v = verdict(
            "com.example",
            "example.com",
            "example.com",
            false,
            vec![],
            vec!["DNS lookup of example.com timed out".into()],
        );
        assert!(v[0].0 == Status::Fail && v[0].1.contains("couldn't confirm"));
        // nothing found at all → what to add, and where
        let v = verdict(
            "com.example.api",
            "api.example.com",
            "api.example.com",
            false,
            vec![],
            vec![],
        );
        assert!(v[0].1.contains("at the apex of api.example.com"));
        assert!(
            v[0].1
                .contains("https://api.example.com/.well-known/mcp-registry-auth")
        );
    }

    use super::*;

    // RFC 8032 test vector 1 public key (32 bytes), and the P-384 generator
    // point compressed (49 bytes, leading 0x03: G's y is odd).
    const ED: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    const P384_G: &str = "A6qHyiK+iwU3jrHHHvMgrXRuHTtii6ebmFn3QeCCVCo4VQLyXb9VKWw6VF44cnYKtw==";

    #[test]
    fn ed25519_and_p384_keys_parse_with_a_fingerprint() {
        assert_eq!(
            parse_key("ed25519", ED).unwrap().fingerprint,
            "ed25519:11qYAYKx"
        );
        assert!(
            parse_key("ecdsap384", P384_G)
                .unwrap()
                .fingerprint
                .starts_with("ecdsap384:")
        );
    }

    #[test]
    fn bad_keys_fail_with_the_registrys_messages() {
        assert_eq!(
            parse_key("ed25519", "AAAA").unwrap_err(),
            "invalid Ed25519 public key size"
        );
        assert!(
            parse_key("ed25519", "not base64!")
                .unwrap_err()
                .starts_with("failed to decode public key")
        );
        assert_eq!(
            parse_key("rsa", ED).unwrap_err(),
            "unsupported public key algorithm: rsa"
        );
        // 49 bytes but uncompressed-style prefix
        let mut b = STD_ENCODING.decode(P384_G).unwrap();
        b[0] = 0x04;
        let bad = base64::engine::general_purpose::STANDARD.encode(&b);
        assert!(
            parse_key("ecdsap384", &bad)
                .unwrap_err()
                .contains("must be compressed")
        );
        // right prefix, not on the curve
        let off = base64::engine::general_purpose::STANDARD.encode([0x02u8; 49]);
        assert!(matches!(
            parse_key("ecdsap384", &off).unwrap_err().as_str(),
            "failed to decompress ECDSA P-384 public key"
        ));
    }

    #[test]
    fn trailing_bits_are_accepted_as_go_does() {
        // "AB==" decodes to one byte; the low bits of 'B' are non-zero. Go's
        // non-strict StdEncoding accepts it; strict decoders reject it.
        assert!(STD_ENCODING.decode("AB==").is_ok());
    }

    #[test]
    fn evaluate_follows_exchange_token() {
        let rec = |s: &str| vec![s.to_string()];
        assert_eq!(evaluate(&rec("v=spf1 -all")), Found::None);
        assert!(
            matches!(evaluate(&rec(&format!("v=MCPv1; k=ed25519; p={ED}"))), Found::Keys(k) if k.len() == 1)
        );
        // one record that fails → its own error
        assert_eq!(
            evaluate(&rec("v=MCPv1; k=ed25519; p=AAAA")),
            Found::Invalid("invalid Ed25519 public key size".into())
        );
        // two that fail → the generic error
        assert_eq!(
            evaluate(&[
                "v=MCPv1; k=ed25519; p=AAAA".into(),
                "v=MCPv1; k=x; p=AAAA".into()
            ]),
            Found::Invalid("no valid MCP public key found".into())
        );
        // a good one next to a stale bad one is enough
        assert!(matches!(
            evaluate(&[
                "v=MCPv1; k=ed25519; p=AAAA".into(),
                format!("v=MCPv1; k=ed25519; p={ED}")
            ]),
            Found::Keys(_)
        ));
    }

    #[test]
    fn whitespace_after_semicolons_is_gos_not_unicode() {
        assert!(matches!(
            evaluate(&[format!("v=MCPv1;\tk=ed25519;\r\np={ED}")]),
            Found::Keys(_)
        ));
        // vertical tab isn't \s in Go
        assert_eq!(
            evaluate(&[format!("v=MCPv1;\x0Bk=ed25519; p={ED}")]),
            Found::None
        );
    }

    #[test]
    fn dns_domains_match_the_registrys_permissions() {
        assert_eq!(dns_domains("com.example"), vec!["example.com"]);
        assert_eq!(
            dns_domains("com.example.api"),
            vec!["api.example.com", "example.com"]
        );
        assert_eq!(
            dns_domains("com.example.v1.api"),
            vec!["api.v1.example.com", "v1.example.com", "example.com"]
        );
        // a single label is never a domain the registry can verify
        assert!(dns_domains("com").is_empty());
        assert_eq!(domain_of("com.example-other"), "example-other.com");
    }

    #[test]
    fn github_pages_and_ips_are_refused() {
        assert!(is_github_pages(&domain_of("io.github.you")));
        assert!(!is_valid_domain("127.0.0.1"));
        assert!(!is_valid_domain("localhost"));
    }
}
