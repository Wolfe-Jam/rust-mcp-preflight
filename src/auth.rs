//! The namespace login check for domain namespaces (`com.example/...`): is a
//! public key published where `mcp-publisher login dns` or `login http` will
//! look for it, and will the registry accept it?
//!
//! Ported from the registry's auth handlers
//! (`internal/api/handlers/v0/auth/`): `common.go` (record pattern, key
//! parsing, permissions), `dns.go` (apex TXT lookup, wrong-selector probe) and
//! `http.go` (`/.well-known/mcp-registry-auth`, no redirects, 4096-byte cap),
//! plus `internal/auth/jwt.go` (which names a login covers). The registry
//! commit it was checked against is in `.github/registry-port.txt`.
//!
//! It reads the published key only. It can't check that your private key
//! matches it, so it prints each key's fingerprint the way the registry does
//! when a signature doesn't verify.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
use hickory_resolver::TokioResolver;
use hickory_resolver::net::NetError;
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
        other => {
            return Err(format!(
                "unsupported public key algorithm: {}",
                printable(other)
            ));
        }
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
/// valid keys, or "no valid MCP public key found". Every record in a string
/// counts, not just the first (an HTTP key file, or one TXT string, can hold
/// several), so a stale record left in front of the new one while changing
/// keys doesn't hide it.
pub fn evaluate(records: &[String]) -> Found {
    let pattern = record_pattern();
    let parsed: Vec<Result<Key, String>> = records
        .iter()
        .flat_map(|r| pattern.captures_iter(r))
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

/// Text from someone else's record, made safe to print: control characters
/// escaped (no terminal escapes reach the user or an MCP client) and at most
/// 40 characters.
fn printable(s: &str) -> String {
    escaped(s, 40)
}

/// `s` with control characters escaped, cut to `max` characters.
fn escaped(s: &str, max: usize) -> String {
    let shown: String = s.chars().take(max).flat_map(char::escape_debug).collect();
    if s.chars().count() > max {
        format!("{shown}...")
    } else {
        shown
    }
}

/// An error and its causes, one after another: reqwest's own message
/// ("error sending request") leaves out why. Escaped, since a TLS error can
/// quote a certificate's names.
fn with_causes(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        let text = c.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        cause = c.source();
    }
    escaped(&out, 300)
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
        Err(e) => lookup_error(&e, name),
    }
}

/// What a failed TXT lookup means. A real "no records" answer (NODATA or
/// NXDOMAIN) carries the zone's SOA (RFC 2308) and means no record. One
/// without an SOA comes from a filtering or broken resolver, so it says
/// nothing about the record.
fn lookup_error(e: &NetError, name: &str) -> Result<Vec<String>, String> {
    if !e.is_no_records_found() {
        return Err(format!("DNS lookup of {name} failed: {e}"));
    }
    if e.clone().into_soa().is_some() {
        return Ok(Vec::new());
    }
    Err(format!(
        "the resolver answered for {name} with no records and no SOA, so it may be filtering TXT records"
    ))
}

/// Addresses the registry refuses to fetch from (http.go `isBlockedIP`):
/// loopback, private (RFC 1918, ULA), link-local, multicast, unspecified,
/// CGNAT (100.64.0.0/10), and the IPv6 ranges that embed an IPv4 address or
/// route to a site (6to4, NAT64, site-local). IPv4-mapped IPv6 is judged as
/// its IPv4 address.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_blocked_v4(v4),
            None => is_blocked_v6(v6),
        },
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || (a == 100 && (64..128).contains(&b))
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10
        || s[0] == 0x2002 // 6to4 2002::/16
        || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] // NAT64 64:ff9b::/96
        || s[..3] == [0x64, 0xff9b, 1] // NAT64 local-use 64:ff9b:1::/48
}

/// The client the key is fetched with: the registry's `Accept: text/plain`,
/// a 10-second budget for one attempt, and no redirects.
fn key_client() -> reqwest::ClientBuilder {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/plain"),
    );
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        // A proxy would resolve the domain itself, past the address check.
        .no_proxy()
}

/// `httpKeyFetchAttempts`, `httpKeyFetchTimeout` (one attempt, lookup
/// included), `httpKeyFetchOverallTimeout` and `httpKeyFetchRetryBackoff` in
/// http.go.
const HTTP_KEY_ATTEMPTS: u32 = 2;
const HTTP_KEY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_KEY_OVERALL_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_KEY_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// How rustls's `Error::InvalidCertificate` reads, the way the error
/// arrives through reqwest.
const CERTIFICATE_ERROR: &str = "invalid peer certificate";

/// A failed key fetch, and whether http.go would try again: yes for no
/// connection, a timeout, a cut-off body, HTTP 408, 429 or 5xx; no for
/// anything about the request itself.
#[derive(Debug, PartialEq)]
struct FetchError {
    why: String,
    retry: bool,
}

impl FetchError {
    fn retry(why: String) -> Self {
        Self { why, retry: true }
    }
    fn fatal(why: String) -> Self {
        Self { why, retry: false }
    }
}

/// The HTTP key, read as http.go reads it: a valid domain, public addresses
/// only, no redirects, 200 only, at most 4096 bytes, trimmed, and a temporary
/// failure tried once more. `Ok(None)` for a 404 (nothing published).
async fn http_key(domain: &str) -> Result<Option<String>, String> {
    if !is_valid_domain(domain) {
        return Err(format!(
            "{domain} isn't a domain the registry accepts for `login http`"
        ));
    }
    let url = format!("https://{domain}/.well-known/mcp-registry-auth");
    with_retry(
        &url,
        HTTP_KEY_OVERALL_TIMEOUT,
        HTTP_KEY_RETRY_BACKOFF,
        || async {
            // A lookup that fails is tried again even when the name doesn't
            // exist, which the registry doesn't retry: the system resolver
            // doesn't say "not found" in a portable way. The verdict is the
            // same, half a second later.
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((domain, 443))
                .await
                .map_err(|e| FetchError::retry(format!("{url}: {}", with_causes(&e))))?
                .collect();
            let public = public_addrs(domain, resolved).map_err(FetchError::fatal)?;
            let client = pinned_client(domain, &public).map_err(FetchError::fatal)?;
            read_key(&client, &url).await
        },
    )
    .await
}

/// A duration in seconds, to a tenth when it isn't whole: "10", "4.5".
fn seconds(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        d.as_secs().to_string()
    } else {
        format!("{:.1}", d.as_secs_f64())
    }
}

/// `FetchKey` in http.go: up to `HTTP_KEY_ATTEMPTS` tries, `backoff` apart,
/// each within `HTTP_KEY_ATTEMPT_TIMEOUT` and all within `overall`. A failure
/// that isn't temporary ends it at once. The error says how many attempts
/// were made.
async fn with_retry<F, Fut>(
    url: &str,
    overall: Duration,
    backoff: Duration,
    mut attempt: F,
) -> Result<Option<String>, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, FetchError>>,
{
    let deadline = tokio::time::Instant::now() + overall;
    let mut last = String::new();
    for n in 1..=HTTP_KEY_ATTEMPTS {
        if n > 1 {
            if tokio::time::Instant::now() + backoff >= deadline {
                return Err(format!("{last} (no time left to try again)"));
            }
            tokio::time::sleep(backoff).await;
        }
        let budget = HTTP_KEY_ATTEMPT_TIMEOUT.min(deadline - tokio::time::Instant::now());
        let outcome = tokio::time::timeout(budget, attempt())
            .await
            .unwrap_or_else(|_| {
                Err(FetchError::retry(format!(
                    "{url}: no answer within {} seconds",
                    seconds(budget)
                )))
            });
        match outcome {
            Ok(key) => return Ok(key),
            Err(e) if !e.retry => {
                return Err(if n > 1 {
                    format!("{} (after {n} attempts)", e.why)
                } else {
                    e.why
                });
            }
            Err(e) => last = e.why,
        }
    }
    Err(format!("{last} (after {HTTP_KEY_ATTEMPTS} attempts)"))
}

/// The addresses the registry would dial, or why there are none.
fn public_addrs(domain: &str, resolved: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, String> {
    let public: Vec<SocketAddr> = resolved
        .into_iter()
        .filter(|a| !is_blocked_ip(a.ip()))
        .collect();
    if public.is_empty() {
        return Err(format!(
            "{domain} resolves only to private or loopback addresses, which the registry refuses to fetch from"
        ));
    }
    Ok(public)
}

/// The key client, connecting to `domain` only at `addrs` (the addresses just
/// checked), as the registry pins its dial.
fn pinned_client(domain: &str, addrs: &[SocketAddr]) -> Result<reqwest::Client, String> {
    key_client()
        .resolve_to_addrs(domain, addrs)
        .build()
        .map_err(|e| e.to_string())
}

/// `fetchKeyOnce` in http.go: one fetch of a key URL with the registry's
/// status and size rules, saying whether a failure is worth another try.
async fn read_key(client: &reqwest::Client, url: &str) -> Result<Option<String>, FetchError> {
    let mut resp = client.get(url).send().await.map_err(|e| {
        let why = format!("{url}: {}", with_causes(&e));
        // `isRetryableHTTPFetchError`: never again for a certificate that
        // doesn't verify, known by rustls's message (a test holds it).
        if why.contains(CERTIFICATE_ERROR) {
            FetchError::fatal(why)
        } else {
            FetchError::retry(why)
        }
    })?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if resp.status().is_redirection() {
        return Err(FetchError::fatal(format!(
            "{url} answers HTTP {status} (a redirect); the registry doesn't follow redirects, so serve the key at this URL"
        )));
    }
    if status != 200 {
        let why = format!("{url} answers HTTP {status}; the registry needs 200");
        // `isTransientHTTPStatus`: a timeout, a rate limit or a server error.
        return Err(
            if status == 408 || status == 429 || (500..600).contains(&status) {
                FetchError::retry(why)
            } else {
                FetchError::fatal(why)
            },
        );
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::retry(format!("{url}: {}", with_causes(&e))))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_KEY_RESPONSE {
            return Err(FetchError::fatal(format!(
                "{url} is over {MAX_KEY_RESPONSE} bytes; the registry reads no more than that"
            )));
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

    // HTTP: only the exact domain (no subdomain grant), and only a domain
    // the registry accepts for `login http`.
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
/// the other's problems are information. Nothing proven is always a failure,
/// since the publish could fail; when DNS couldn't be read the failure says
/// the login wasn't checked and to run again, with every reason.
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
    } else if !unchecked.is_empty() {
        let mut out = vec![(
            Status::Fail,
            format!(
                "namespace {namespace}: login not checked: couldn't read DNS ({}). Run again, or from another network, before you publish",
                unchecked.join("; ")
            ),
        )];
        out.extend(problems.into_iter().map(|p| (Status::Fail, p)));
        out
    } else if !problems.is_empty() {
        problems.into_iter().map(|p| (Status::Fail, p)).collect()
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
        // DNS couldn't be read → still a failure (never "Ready"), saying to run again
        let v = verdict(
            "com.example",
            "example.com",
            "example.com",
            false,
            vec![],
            vec!["DNS lookup of example.com timed out".into()],
        );
        assert!(v.len() == 1 && v[0].0 == Status::Fail, "{v:?}");
        assert!(v[0].1.contains("timed out") && v[0].1.contains("Run again"));
        // DNS couldn't be read and HTTP failed (no network) → both reasons, failing
        let v = verdict(
            "com.example",
            "example.com",
            "example.com",
            false,
            http_redirect(),
            vec!["DNS lookup of example.com timed out".into()],
        );
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().all(|(s, _)| *s == Status::Fail), "{v:?}");
        assert!(v[0].1.contains("timed out") && v[1].1.contains("HTTP 301"));
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
        // the same two in one string, as the HTTP key arrives (#1849): the
        // stale one first no longer hides the good one
        let body = format!("v=MCPv1; k=ed25519; p=AAAA\nv=MCPv1; k=ed25519; p={ED}");
        assert!(matches!(evaluate(&[body]), Found::Keys(k) if k.len() == 1));
        // and two bad ones in one string get the generic error, not the first one's
        assert_eq!(
            evaluate(&["v=MCPv1; k=ed25519; p=AAAA v=MCPv1; k=x; p=AAAA".into()]),
            Found::Invalid("no valid MCP public key found".into())
        );
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

    #[test]
    fn a_hostile_algorithm_name_is_printed_escaped_and_short() {
        let err = parse_key("\u{1b}[2J\u{1b}]0;owned\u{7}", ED).unwrap_err();
        assert!(!err.contains('\u{1b}') && !err.contains('\u{7}'), "{err:?}");
        assert!(err.contains("\\u{1b}[2J"), "{err:?}");
        let err = parse_key(&"a".repeat(500), ED).unwrap_err();
        assert!(err.len() < 100 && err.ends_with("..."), "{err}");
    }

    #[test]
    fn private_addresses_are_refused_as_the_registry_refuses_them() {
        let blocked = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2002:a00:1::",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
        ];
        for ip in blocked {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} should be refused");
        }
        let public = [
            "93.184.215.14",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ];
        for ip in public {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip} is public");
        }
    }

    /// A one-response HTTP server on 127.0.0.1; returns its base URL and the
    /// last request it received.
    async fn serve(response: String) -> (String, std::sync::Arc<std::sync::Mutex<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen_by_server = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                *seen_by_server.lock().unwrap() = String::from_utf8_lossy(&req).into_owned();
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/.well-known/mcp-registry-auth"), seen)
    }

    fn response(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn http_key_follows_the_registrys_status_size_and_redirect_rules() {
        let client = key_client().build().unwrap();
        let record = format!("v=MCPv1; k=ed25519; p={ED}");

        // 200: trimmed, and the registry's Accept header was sent
        let (url, seen) = serve(response("200 OK", "", &format!("\n  {record} \n"))).await;
        assert_eq!(read_key(&client, &url).await, Ok(Some(record.clone())));
        assert!(
            seen.lock()
                .unwrap()
                .to_lowercase()
                .contains("accept: text/plain"),
            "no Accept: text/plain"
        );
        // exactly 4096 bytes is read; one more is refused
        let at_cap = format!("{record}{}", " ".repeat(MAX_KEY_RESPONSE - record.len()));
        let (url, _) = serve(response("200 OK", "", &at_cap)).await;
        assert_eq!(read_key(&client, &url).await, Ok(Some(record.clone())));
        let (url, _) = serve(response("200 OK", "", &format!("{at_cap} "))).await;
        assert!(
            read_key(&client, &url)
                .await
                .unwrap_err()
                .why
                .contains("over 4096 bytes")
        );
        // 404: nothing published
        let (url, _) = serve(response("404 Not Found", "", "")).await;
        assert_eq!(read_key(&client, &url).await, Ok(None));
        // a redirect is reported, not followed
        let (url, _) = serve(response(
            "301 Moved Permanently",
            "Location: https://elsewhere.example/key\r\n",
            "",
        ))
        .await;
        assert!(
            read_key(&client, &url)
                .await
                .unwrap_err()
                .why
                .contains("redirect")
        );
        // any other status fails
        let (url, _) = serve(response("406 Not Acceptable", "", "")).await;
        assert!(
            read_key(&client, &url)
                .await
                .unwrap_err()
                .why
                .contains("HTTP 406")
        );
        // which failures http.go tries again: 408, 429 and 5xx; not other
        // statuses, redirects or an oversized body
        for (status, retry) in [
            ("406 Not Acceptable", false),
            ("408 Request Timeout", true),
            ("429 Too Many Requests", true),
            ("503 Service Unavailable", true),
            ("301 Moved Permanently", false),
        ] {
            let (url, _) = serve(response(status, "", "")).await;
            assert_eq!(
                read_key(&client, &url).await.unwrap_err().retry,
                retry,
                "{status}"
            );
        }
        let (url, _) = serve(response("200 OK", "", &format!("{at_cap} "))).await;
        assert!(!read_key(&client, &url).await.unwrap_err().retry);
        // nothing listening: no connection, tried again
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let err = read_key(&client, &format!("http://127.0.0.1:{port}/k"))
            .await
            .unwrap_err();
        assert!(err.retry, "{}", err.why);
        // a response cut off before its length: tried again
        let (url, _) =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nabc".into())
                .await;
        let err = read_key(&client, &url).await.unwrap_err();
        assert!(err.retry, "{}", err.why);
    }

    #[test]
    fn a_certificate_error_still_reads_as_matched() {
        // a rustls update that rewords it would retry bad certificates
        let e = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        assert!(e.to_string().contains(CERTIFICATE_ERROR), "{e}");
    }

    #[tokio::test]
    async fn a_temporary_failure_is_tried_once_more() {
        type Outcome = Result<Option<String>, FetchError>;
        let quick = Duration::from_millis(1);
        let overall = Duration::from_secs(15);
        // runs with_retry over scripted attempts: what it returned, and how
        // many attempts it made
        async fn run(
            overall: Duration,
            backoff: Duration,
            outcomes: Vec<Outcome>,
        ) -> (Result<Option<String>, String>, usize) {
            let left = std::cell::RefCell::new(outcomes.into_iter());
            let made = std::cell::Cell::new(0);
            let got = with_retry("u", overall, backoff, || {
                made.set(made.get() + 1);
                let next = left.borrow_mut().next().expect("an attempt too many");
                async move { next }
            })
            .await;
            (got, made.get())
        }
        let temporary = |why: &str| Err(FetchError::retry(why.into()));
        let lasting = |why: &str| Err(FetchError::fatal(why.into()));

        // a temporary failure, then the key
        assert_eq!(
            run(
                overall,
                quick,
                vec![temporary("u answers HTTP 503"), Ok(Some("key".into()))]
            )
            .await,
            (Ok(Some("key".into())), 2)
        );
        // a failure that isn't temporary: one attempt, its own message
        assert_eq!(
            run(overall, quick, vec![lasting("u answers HTTP 406")]).await,
            (Err("u answers HTTP 406".into()), 1)
        );
        // temporary twice: the last failure, and how many attempts
        assert_eq!(
            run(
                overall,
                quick,
                vec![
                    temporary("u answers HTTP 503"),
                    temporary("u answers HTTP 502")
                ]
            )
            .await,
            (Err("u answers HTTP 502 (after 2 attempts)".into()), 2)
        );
        // temporary, then not: stops there, and says it was the second try
        assert_eq!(
            run(
                overall,
                quick,
                vec![
                    temporary("u answers HTTP 503"),
                    lasting("u answers HTTP 406")
                ]
            )
            .await,
            (Err("u answers HTTP 406 (after 2 attempts)".into()), 2)
        );
        // no time left for the backoff: no second attempt
        assert_eq!(
            run(
                quick,
                Duration::from_secs(1),
                vec![temporary("u answers HTTP 503")]
            )
            .await,
            (
                Err("u answers HTTP 503 (no time left to try again)".into()),
                1
            )
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempt_that_hangs_times_out_within_the_overall_budget() {
        // first attempt hangs: 10 s; the second gets what's left of 15 s
        let got = with_retry(
            "u",
            HTTP_KEY_OVERALL_TIMEOUT,
            HTTP_KEY_RETRY_BACKOFF,
            std::future::pending,
        )
        .await;
        assert_eq!(
            got,
            Err("u: no answer within 4.5 seconds (after 2 attempts)".into())
        );
    }

    #[test]
    fn a_domain_on_a_private_address_is_refused_before_fetching() {
        let addr = |a: &str| a.parse::<SocketAddr>().unwrap();
        let err = public_addrs(
            "example.com",
            vec![addr("127.0.0.1:443"), addr("[fd00::1]:443")],
        )
        .unwrap_err();
        assert!(err.contains("private or loopback"), "{err}");
        // a mixed answer keeps only the public address, as the registry dials it
        assert_eq!(
            public_addrs(
                "example.com",
                vec![addr("10.0.0.1:443"), addr("93.184.215.14:443")]
            ),
            Ok(vec![addr("93.184.215.14:443")])
        );
    }

    #[test]
    fn an_empty_answer_counts_as_no_record_only_with_an_soa() {
        use hickory_resolver::net::NoRecords;
        use hickory_resolver::proto::op::{Query, ResponseCode};
        use hickory_resolver::proto::rr::rdata::SOA;
        use hickory_resolver::proto::rr::{Name, Record};
        for code in [ResponseCode::NoError, ResponseCode::NXDomain] {
            let bare = NoRecords::new(Query::new(), code);
            let msg = lookup_error(&NetError::from(bare.clone()), "example.com").unwrap_err();
            assert!(msg.contains("no SOA"), "{code:?}: {msg}");
            let mut with_soa = bare;
            with_soa.soa = Some(Box::new(Record::from_rdata(
                Name::root(),
                60,
                SOA::new(Name::root(), Name::root(), 1, 1, 1, 1, 1),
            )));
            assert_eq!(
                lookup_error(&NetError::from(with_soa), "example.com"),
                Ok(Vec::new()),
                "{code:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_invalid_domain_is_refused_before_any_lookup() {
        let err = http_key("-bad.example").await.unwrap_err();
        assert!(err.contains("isn't a domain the registry accepts"), "{err}");
    }

    #[tokio::test]
    async fn the_key_is_fetched_only_from_the_checked_addresses() {
        // `pinned.invalid` doesn't resolve: the fetch reaches the test server
        // only because the client is pinned to its address.
        let (url, _) = serve(response("200 OK", "", "pinned")).await;
        let addr: SocketAddr = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let client = pinned_client("pinned.invalid", &[addr]).unwrap();
        let pinned = format!(
            "http://pinned.invalid:{}/.well-known/mcp-registry-auth",
            addr.port()
        );
        assert_eq!(
            read_key(&client, &pinned).await,
            Ok(Some("pinned".to_string()))
        );
    }
}
