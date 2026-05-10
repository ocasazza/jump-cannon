//! Pure-string type detectors for the metadata modal — wikilinks,
//! URLs, ISO-8601 dates, JIRA-style ticket ids. Each returns the
//! parsed payload (or a `bool`) so the frontmatter renderer can pick
//! a badge variant without re-parsing.

pub(crate) fn parse_wikilink(s: &str) -> Option<(String, Option<String>)> {
    let s = s.trim();
    if !(s.starts_with("[[") && s.ends_with("]]") && s.len() >= 5) {
        return None;
    }
    let inner = &s[2..s.len() - 2];
    if inner.is_empty() {
        return None;
    }
    if let Some((page, alias)) = inner.split_once('|') {
        Some((page.trim().to_string(), Some(alias.trim().to_string())))
    } else {
        Some((inner.trim().to_string(), None))
    }
}

pub(crate) fn host_from_url(s: &str) -> Option<String> {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    let host = rest.split(|c: char| c == '/' || c == ':' || c == '?').next()?;
    Some(host.to_string())
}

pub(crate) fn is_url(s: &str) -> bool {
    (s.starts_with("http://") || s.starts_with("https://"))
        && !s.contains(char::is_whitespace)
        && s.len() < 2048
}

pub(crate) fn is_iso_date(s: &str) -> bool {
    // YYYY-MM-DD with optional time tail. Strict on the date head.
    if s.len() < 10 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
        && (s.len() == 10 || matches!(bytes[10], b'T' | b' '))
}

/// Parse a JIRA-style ticket id at the start of the string. Accepts:
///   FOO-123
///   FOO-123: subject
///   FOO-123 — subject
/// Returns the bare id (e.g. "FOO-123") or None.
pub(crate) fn parse_ticket_id(s: &str) -> Option<String> {
    let head: &str = s.split(|c: char| c == ':' || c == ' ').next()?;
    if head.len() < 3 || head.len() > 24 {
        return None;
    }
    let (prefix, rest) = head.split_once('-')?;
    if prefix.is_empty() || prefix.len() > 16 {
        return None;
    }
    let mut chars = prefix.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return None;
    }
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(head.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilink_basic() {
        assert_eq!(
            parse_wikilink("[[Alpha]]"),
            Some(("Alpha".into(), None))
        );
        assert_eq!(
            parse_wikilink("[[Alpha|alias]]"),
            Some(("Alpha".into(), Some("alias".into())))
        );
        assert_eq!(parse_wikilink("Alpha"), None);
        assert_eq!(parse_wikilink("[[]]"), None);
    }

    #[test]
    fn url_detect() {
        assert!(is_url("https://example.com"));
        assert!(is_url("http://luna.local:3000/d/x"));
        assert!(!is_url("example.com"));
        assert!(!is_url("not a url"));
    }

    #[test]
    fn iso_date_detect() {
        assert!(is_iso_date("2026-04-30"));
        assert!(is_iso_date("2026-04-30T12:00:00Z"));
        assert!(!is_iso_date("April 30"));
        assert!(!is_iso_date("2026-4-1"));
    }

    #[test]
    fn ticket_id_detect() {
        assert_eq!(parse_ticket_id("ITHELP-1234"), Some("ITHELP-1234".into()));
        assert_eq!(parse_ticket_id("JIRA-42: subject"), Some("JIRA-42".into()));
        assert_eq!(parse_ticket_id("FOO-7 — body"), Some("FOO-7".into()));
        assert_eq!(parse_ticket_id("not-a-ticket"), None);
        assert_eq!(parse_ticket_id("foo-1"), None);
    }
}
