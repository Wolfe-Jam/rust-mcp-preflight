//! The `mcp-name:` ownership token, matched the way the registry matches it.
//!
//! Ported from modelcontextprotocol/registry
//! `internal/validators/registries/mcpname.go` (commit 04623ed, 2026-06-05),
//! shared there by the PyPI, NuGet and Cargo validators. A README this module
//! accepts is one the registry accepts.

/// The server-name pattern from the registry schema.
pub fn is_valid_server_name(name: &str) -> bool {
    let Some((ns, rest)) = name.split_once('/') else {
        return false;
    };
    !ns.is_empty()
        && !rest.is_empty()
        && ns
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        && rest
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-')
}

/// A byte that can continue a server name: `[A-Za-z0-9._/-]`.
fn is_server_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'/')
}

/// The text after a matched name ends the token: end of text, any byte that
/// can't continue a name, or an HTML comment close (`-->` / `--!>`).
fn is_boundary(rest: &str) -> bool {
    match rest.as_bytes().first() {
        None => true,
        Some(&c) if !is_server_name_char(c) => true,
        _ => rest.starts_with("-->") || rest.starts_with("--!>"),
    }
}

/// True when `content` holds `mcp-name: <server_name>` as a complete token,
/// not as the prefix of a longer name.
pub fn contains_token(content: &str, server_name: &str) -> bool {
    let token = format!("mcp-name: {server_name}");
    let mut from = 0;
    while let Some(idx) = content[from..].find(&token) {
        let end = from + idx + token.len();
        if is_boundary(&content[end..]) {
            return true;
        }
        from += idx + 1;
    }
    false
}

/// When the token is present but glued to the next character, that character.
pub fn glued_trailing(content: &str, server_name: &str) -> Option<char> {
    let token = format!("mcp-name: {server_name}");
    let idx = content.find(&token)?;
    content[idx + token.len()..].chars().next()
}

/// Every `mcp-name:` value in the text, so a near miss can be named
/// (a typo, the wrong case, another server's name).
pub fn names_found(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = content[from..].find("mcp-name:") {
        let start = from + idx + "mcp-name:".len();
        let value: String = content[start..]
            .trim_start_matches(' ')
            .bytes()
            .take_while(|&c| is_server_name_char(c))
            .map(char::from)
            .collect();
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
        from = start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "io.github.acme/widget";

    #[test]
    fn plain_line_matches() {
        assert!(contains_token("mcp-name: io.github.acme/widget\n", NAME));
    }

    #[test]
    fn end_of_text_matches() {
        assert!(contains_token("mcp-name: io.github.acme/widget", NAME));
    }

    #[test]
    fn rendered_code_tag_matches() {
        // crates.io renders `mcp-name: …` in backticks as <code>…</code>.
        assert!(contains_token(
            "<li><code>mcp-name: io.github.acme/widget</code></li>",
            NAME
        ));
    }

    #[test]
    fn longer_name_is_not_a_match() {
        assert!(!contains_token("mcp-name: io.github.acme/widget-pro", NAME));
        assert_eq!(
            glued_trailing("mcp-name: io.github.acme/widget-pro", NAME),
            Some('-')
        );
    }

    #[test]
    fn later_complete_token_still_matches() {
        assert!(contains_token(
            "mcp-name: io.github.acme/widget-pro\nmcp-name: io.github.acme/widget\n",
            NAME
        ));
    }

    #[test]
    fn comment_close_without_space_is_a_boundary() {
        assert!(contains_token(
            "<!-- mcp-name: io.github.acme/widget-->",
            NAME
        ));
        assert!(contains_token(
            "<!-- mcp-name: io.github.acme/widget--!>",
            NAME
        ));
    }

    #[test]
    fn non_ascii_after_name_is_a_boundary() {
        assert!(contains_token("mcp-name: io.github.acme/widget—ok", NAME));
    }

    #[test]
    fn missing_token() {
        assert!(!contains_token("no token here", NAME));
        assert_eq!(glued_trailing("no token here", NAME), None);
    }

    #[test]
    fn finds_near_misses() {
        let html =
            "<code>mcp-name: io.github.Acme/widget</code> mcp-name: io.github.acme/widget-pro";
        assert_eq!(
            names_found(html),
            vec!["io.github.Acme/widget", "io.github.acme/widget-pro"]
        );
    }

    #[test]
    fn server_name_pattern() {
        assert!(is_valid_server_name("io.github.acme/widget"));
        assert!(is_valid_server_name("com.example/my_server.v2"));
        assert!(!is_valid_server_name("widget"));
        assert!(!is_valid_server_name("io.github.acme/"));
        assert!(!is_valid_server_name("io_github/acme"));
        assert!(!is_valid_server_name("io.github.acme/wid/get"));
    }
}
