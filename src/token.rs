//! The `mcp-name:` ownership token, matched the way the registry matches it.
//!
//! Ported from modelcontextprotocol/registry
//! `internal/validators/registries/mcpname.go`, shared there by the PyPI,
//! NuGet and Cargo validators. A README this module accepts is one the
//! registry accepts. The registry commit it was checked against, and every
//! file ported, are in `.github/registry-port.txt`.

/// Why a server name would be refused at publish, or `None` if it's fine.
///
/// Mirrors the registry's `parseServerName` (`internal/validators/validators.go`):
/// one slash; the namespace matches `[a-zA-Z0-9][a-zA-Z0-9.-]*[a-zA-Z0-9]` and the
/// name `[a-zA-Z0-9][a-zA-Z0-9._-]*[a-zA-Z0-9]`, so each side is at least two
/// characters and starts and ends with a letter or digit. (Stricter than the
/// schema's pattern, which is not what publish checks.)
pub fn server_name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("server name is required".into());
    }
    let Some((ns, rest)) = name.split_once('/') else {
        return Some("server name must be namespace/name (e.g. io.github.you/widget)".into());
    };
    if rest.contains('/') {
        return Some("server name can't have more than one slash".into());
    }
    if ns.is_empty() || rest.is_empty() {
        return Some("both the namespace and the name must be non-empty".into());
    }
    let part_ok = |part: &str, middle: &dyn Fn(u8) -> bool| {
        let b = part.as_bytes();
        b.len() >= 2
            && b[0].is_ascii_alphanumeric()
            && b[b.len() - 1].is_ascii_alphanumeric()
            && b.iter().all(|&c| c.is_ascii_alphanumeric() || middle(c))
    };
    if !part_ok(ns, &|c| c == b'.' || c == b'-') {
        return Some(format!(
            "namespace '{ns}' is invalid: it must be at least two characters, start and end with a letter or digit, and may contain dots and hyphens in the middle"
        ));
    }
    if !part_ok(rest, &|c| c == b'.' || c == b'_' || c == b'-') {
        return Some(format!(
            "name '{rest}' is invalid: it must be at least two characters, start and end with a letter or digit, and may contain dots, underscores and hyphens in the middle"
        ));
    }
    None
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

/// `mcp-name:` lines whose spacing the registry won't accept (no space, or
/// more than one), with the value and the number of spaces.
pub fn spacing_misses(content: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = content[from..].find("mcp-name:") {
        let start = from + idx + "mcp-name:".len();
        let rest = &content[start..];
        let spaces = rest.bytes().take_while(|&c| c == b' ').count();
        let value: String = rest[spaces..]
            .bytes()
            .take_while(|&c| is_server_name_char(c))
            .map(char::from)
            .collect();
        if spaces != 1 && !value.is_empty() {
            out.push((value, spaces));
        }
        from = start;
    }
    out
}

/// Every `mcp-name:` value in the text, so a near miss can be named
/// (a typo, the wrong case, another server's name).
pub fn names_found(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = content[from..].find("mcp-name:") {
        let start = from + idx + "mcp-name:".len();
        // The registry's token has exactly one space after the colon.
        let value: String = content[start..]
            .strip_prefix(' ')
            .unwrap_or("")
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
    fn server_name_rules_match_publish() {
        for ok in [
            "io.github.acme/widget",
            "com.example/my_server.v2",
            "ab/cd",
            "io.github.Wolfe-Jam/rust-mcp-preflight",
        ] {
            assert_eq!(server_name_problem(ok), None, "{ok}");
        }
        for bad in [
            "widget",
            "a/b",
            "io.github.acme/w",
            "io.github.acme/widget-",
            "io.github.acme-/widget",
            "-acme/widget",
            "io_github/acme",
            "io.github.acme/wid/get",
            "io.github.acme/",
        ] {
            assert!(server_name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn spacing_misses_are_named() {
        assert_eq!(
            spacing_misses("mcp-name:io.github.acme/widget and mcp-name:  io.github.acme/w2"),
            vec![
                ("io.github.acme/widget".to_string(), 0),
                ("io.github.acme/w2".to_string(), 2)
            ]
        );
        assert!(spacing_misses("mcp-name: io.github.acme/widget").is_empty());
    }

    #[test]
    fn near_misses_need_exactly_one_space() {
        assert!(names_found("mcp-name:  io.github.acme/widget").is_empty());
        assert!(names_found("mcp-name:io.github.acme/widget").is_empty());
    }
}
