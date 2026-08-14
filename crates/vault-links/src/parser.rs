use std::collections::HashMap;
use std::path::Path;

/// The parsed representation of a single vault note.
#[derive(Debug, Default, Clone)]
pub struct ParsedNote {
    pub title: String,
    pub tags: Vec<String>,
    pub frontmatter: HashMap<String, serde_json::Value>,
    pub doctype: Option<String>,
    pub links: Vec<String>,
    /// Markdown body without YAML frontmatter. Retained for the generic
    /// importer-driven full-text index.
    pub body: String,
}

/// Parse a markdown file's raw text into a `ParsedNote`.
pub fn parse_note(path: &Path, text: &str) -> ParsedNote {
    parse_note_impl(path, text, false).expect("non-strict note parsing is infallible")
}

/// Parse a markdown file while rejecting malformed YAML frontmatter.
///
/// The compatibility [`parse_note`] entrypoint preserves its historical
/// best-effort behavior. Importers use this fallible variant so a malformed
/// note cannot be silently published with its metadata stripped.
pub fn try_parse_note(path: &Path, text: &str) -> Result<ParsedNote, serde_yaml::Error> {
    parse_note_impl(path, text, true)
}

fn parse_note_impl(
    path: &Path,
    text: &str,
    reject_invalid_frontmatter: bool,
) -> Result<ParsedNote, serde_yaml::Error> {
    let (fm_raw, body) = split_frontmatter(text);
    let mut frontmatter: HashMap<String, serde_json::Value> = HashMap::new();
    let mut tags: Vec<String> = Vec::new();
    let mut doctype: Option<String> = None;

    if let Some(fm) = fm_raw {
        let parsed = match serde_yaml::from_str::<serde_yaml::Value>(&fm) {
            Ok(value) => Some(value),
            Err(error) if reject_invalid_frontmatter => return Err(error),
            Err(_) => None,
        };
        if let Some(serde_yaml::Value::Mapping(m)) = parsed {
            for (k, val) in &m {
                let key = k.as_str().unwrap_or("").to_string();
                let json_val = yaml_to_json(val);
                // Obsidian's `tags:` field shows up in many shapes in
                // the wild. The legacy code only accepted the YAML
                // array form (`tags: [a, b]`), silently dropping
                // every other form — which meant the API returned
                // empty `tags` for most vaults, the renderer's
                // `meta.tags.is_empty()` guard hid the badges, and
                // the user saw a chip-less sidebar.
                //
                // Accept (in order of common usage):
                //   - YAML array of strings: `tags: [a, b]`
                //   - Comma-separated scalar: `tags: a, b`
                //   - Single scalar:          `tags: a`
                // Also accept the singular `tag:` form some users
                // type. Strip leading `#` and surrounding quotes
                // (the `clean_tag` shape).
                if key == "tags" || key == "tag" {
                    for t in extract_tags_from_value(&json_val) {
                        if !tags.contains(&t) {
                            tags.push(t);
                        }
                    }
                }
                if key == "doctype" {
                    doctype = json_val
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_string);
                }
                frontmatter.insert(key, json_val);
            }
        }
    }

    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let links = extract_wikilinks(&body);

    // Inline `#tag` mentions in the body. Obsidian users mix frontmatter
    // and inline tags freely; both should surface in the sidebar.
    for t in extract_inline_tags(&body) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }

    Ok(ParsedNote {
        title,
        tags,
        frontmatter,
        doctype,
        links,
        body,
    })
}

/// Pull tag strings out of an arbitrary frontmatter JSON value.
/// Accepts array-of-strings, comma-separated string, single string.
/// Strips leading `#` and quotes, drops empties.
fn extract_tags_from_value(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        serde_json::Value::Array(arr) => {
            for el in arr {
                out.extend(extract_tags_from_value(el));
            }
        }
        serde_json::Value::String(s) => {
            // Comma-separated string: `tags: foo, bar`. Single-tag case
            // (no comma) falls through naturally as one-element split.
            for piece in s.split(',') {
                let cleaned = clean_tag(piece);
                if !cleaned.is_empty() {
                    out.push(cleaned);
                }
            }
        }
        _ => {}
    }
    out
}

fn clean_tag(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('#')
        .to_string()
}

/// Scan markdown body for inline `#tag` tokens (Obsidian's own format).
/// Matches `#word-chars` where word-chars are alnum / `_` / `-` / `/`.
/// Skips `#` that lives inside code spans / fenced blocks (cheap heuristic:
/// drop the entire line if it starts with four+ leading spaces or sits
/// inside a triple-backtick fence). Header lines (`#`, `##`, ...) are
/// excluded by requiring at least one non-space char before the `#`, or
/// alternatively a non-space directly after the `#` that isn't another `#`.
fn extract_inline_tags(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Markdown ATX headings: skip — the leading `#`s aren't tags.
        // (No false positives because a heading line starts with `#` then a space.)
        if let Some(rest) = trimmed.strip_prefix('#') {
            // Allow `#####` headings: drop more leading `#`s then require space.
            let rest = rest.trim_start_matches('#');
            if rest.starts_with(' ') {
                continue;
            }
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'#' {
                // Must be at start of line OR preceded by whitespace /
                // punctuation. Avoids matching `foo#bar` (anchor link
                // or path fragment).
                let preceded_ok = i == 0
                    || matches!(
                        bytes[i - 1],
                        b' ' | b'\t' | b',' | b';' | b'(' | b'[' | b'{'
                    );
                let mut j = i + 1;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'/') {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if preceded_ok && j > i + 1 {
                    // Need at least one alphabetic char (skip `#1`,
                    // `#123` — usually issue numbers / step counters,
                    // not tags).
                    let token = &line[i + 1..j];
                    if token.chars().any(|c| c.is_ascii_alphabetic()) {
                        let t = token.to_string();
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Split `---\n…\n---\n` frontmatter from the rest of the file.
/// Returns `(Some(frontmatter_text), body)` or `(None, full_text)`.
fn split_frontmatter(text: &str) -> (Option<String>, String) {
    let rest = match text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => return (None, text.to_string()),
    };

    // Find closing fence on its own line.
    if let Some(end) = find_closing_fence(rest) {
        let fm = rest[..end].to_string();
        let after = &rest[end..];
        let after = after
            .strip_prefix("---\n")
            .or_else(|| after.strip_prefix("---\r\n"))
            .or_else(|| after.strip_prefix("---"))
            .unwrap_or(after);
        return (Some(fm), after.to_string());
    }

    (None, text.to_string())
}

fn find_closing_fence(s: &str) -> Option<usize> {
    let mut start = 0;
    while start < s.len() {
        let line_end = s[start..].find('\n').map(|i| start + i).unwrap_or(s.len());
        let line = s[start..line_end].trim_end_matches('\r');
        if line == "---" {
            return Some(start);
        }
        start = line_end + 1;
    }
    None
}

/// Extract `[[wikilink]]` targets from arbitrary markdown text.
///
/// - `[[Page|alias]]` → `"Page"`
/// - `[[Page#Heading]]` → `"Page"`
/// - Embedded images `![[img.png]]` are also captured (the `!` is before the
///   `[[` and is not part of the link text, so they work naturally).
pub fn extract_wikilinks(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i + 1 < len {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            i += 2;
            let start = i;
            // Collect until `]]` or end of string
            while i + 1 < len && !(bytes[i] == b']' && bytes[i + 1] == b']') {
                i += 1;
            }
            let inner = &text[start..i];
            // Skip closing ]]
            i += 2;
            // Strip display alias: [[target|alias]] → target
            let target = inner.split('|').next().unwrap_or(inner).trim();
            // Strip heading anchor: [[Page#Heading]] → Page
            let target = target.split('#').next().unwrap_or(target).trim();
            if !target.is_empty() {
                links.push(target.to_string());
            }
        } else {
            i += 1;
        }
    }
    links
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::json!(f)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => {
            let map = m
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|ks| (ks.to_string(), yaml_to_json(v))))
                .collect();
            serde_json::Value::Object(map)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(&t.value),
    }
}
