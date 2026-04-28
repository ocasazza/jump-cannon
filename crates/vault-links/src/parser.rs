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
}

/// Parse a markdown file's raw text into a `ParsedNote`.
pub fn parse_note(path: &Path, text: &str) -> ParsedNote {
    let (fm_raw, body) = split_frontmatter(text);
    let mut frontmatter: HashMap<String, serde_json::Value> = HashMap::new();
    let mut tags: Vec<String> = Vec::new();
    let mut doctype: Option<String> = None;

    if let Some(fm) = fm_raw {
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&fm) {
            if let serde_yaml::Value::Mapping(m) = &v {
                for (k, val) in m {
                    let key = k.as_str().unwrap_or("").to_string();
                    let json_val = yaml_to_json(val);
                    if key == "tags" {
                        if let serde_json::Value::Array(arr) = &json_val {
                            tags = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                        }
                    }
                    if key == "doctype" {
                        doctype = json_val.as_str().map(|s| s.to_string());
                    }
                    frontmatter.insert(key, json_val);
                }
            }
        }
    }

    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let links = extract_wikilinks(&body);

    ParsedNote {
        title,
        tags,
        frontmatter,
        doctype,
        links,
    }
}

/// Split `---\n…\n---\n` frontmatter from the rest of the file.
/// Returns `(Some(frontmatter_text), body)` or `(None, full_text)`.
fn split_frontmatter(text: &str) -> (Option<String>, String) {
    let rest = match text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) {
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
