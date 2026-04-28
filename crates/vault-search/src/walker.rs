use anyhow::{Context, Result};
use ignore::{overrides::OverrideBuilder, WalkBuilder};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One discovered note in the vault. Body is already frontmatter-stripped
/// and capped at the configured size at parse time.
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub mtime: u64,
}

/// Maximum body size indexed per note. 64 KiB matches the spec — the goal
/// is FTS, not document storage; longer notes get truncated.
const BODY_CAP: usize = 64 * 1024;

/// Build a `WalkBuilder` honoring the canonical exclusion contract.
///
/// This intentionally diverges from `vault-links`'s broken non-recursive
/// `--glob '!.obsidian/**'` (which only matched a top-level directory) by
/// using `ignore`'s override globs that DO recurse. Callers can opt back
/// into `_hippo/**` via `include_hippo = true`.
pub fn build_walker(vault: &Path, include_hippo: bool) -> Result<ignore::Walk> {
    let mut overrides = OverrideBuilder::new(vault);

    // The exclusion contract. Each `!pattern` is a "do NOT include" override.
    // The `**/` prefix forces match at any depth, fixing the vault-links bug.
    let excludes = [
        "!**/.obsidian/**",
        "!**/.git/**",
        "!**/.jj/**",
        "!**/Excalidraw/**",
        "!**/Ink/**",
        "!**/*.base",
        "!**/*.canvas",
    ];
    for pat in excludes {
        overrides.add(pat).with_context(|| format!("override pat: {pat}"))?;
    }
    if !include_hippo {
        overrides.add("!**/_hippo/**").context("override pat _hippo")?;
    }

    let overrides = overrides.build().context("build overrides")?;

    let walker = WalkBuilder::new(vault)
        .hidden(false) // .obsidian is hidden on some FSes; we want explicit control
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .overrides(overrides)
        .build();

    Ok(walker)
}

/// Walk the vault and yield (rel_id, abs_path, mtime) for each markdown file
/// that survives the exclusion contract. mtime is seconds-since-epoch.
pub fn list_markdown(vault: &Path, include_hippo: bool) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    for entry in build_walker(vault, include_hippo)? {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, "walk error");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = match path.strip_prefix(vault) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let id = rel
            .with_extension("")
            .to_string_lossy()
            .to_string()
            .replace('\\', "/");
        let mtime = path
            .metadata()
            .and_then(|m| m.modified())
            .map(systime_to_secs)
            .unwrap_or(0);
        out.push((id, path.to_path_buf(), mtime));
    }
    Ok(out)
}

fn systime_to_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read a markdown file and turn it into an indexable `Note`. Strips an
/// optional `---\n…\n---` frontmatter block, pulls out a title (first H1
/// or basename fallback), best-effort-extracts `tags:` from the
/// frontmatter, and caps the body at `BODY_CAP`.
pub fn parse_note(id: &str, path: &Path, mtime: u64) -> Result<Note> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
    let (frontmatter, body) = split_frontmatter(&raw);
    let tags = extract_tags(frontmatter);
    let title = first_h1(body).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    });
    let body = if body.len() > BODY_CAP {
        // Use char boundary-safe truncation.
        let mut end = BODY_CAP;
        while !body.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        body[..end].to_string()
    } else {
        body.to_string()
    };
    Ok(Note {
        id: id.to_string(),
        title,
        body,
        tags,
        mtime,
    })
}

/// Returns (frontmatter_block_without_fences, remaining_body).
fn split_frontmatter(raw: &str) -> (&str, &str) {
    // Must start with `---\n` (or `---\r\n`) to count as frontmatter.
    let trimmed = raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n"));
    let Some(rest) = trimmed else {
        return ("", raw);
    };
    // Find the closing fence on its own line.
    if let Some(end_idx) = find_closing_fence(rest) {
        let fm = &rest[..end_idx];
        // Skip past the closing fence line.
        let after = &rest[end_idx..];
        let after = after.strip_prefix("---\n")
            .or_else(|| after.strip_prefix("---\r\n"))
            .or_else(|| after.strip_prefix("---"))
            .unwrap_or(after);
        (fm, after)
    } else {
        // No closing fence — treat the whole file as body.
        ("", raw)
    }
}

fn find_closing_fence(s: &str) -> Option<usize> {
    let mut start = 0;
    while start < s.len() {
        let line_end = s[start..].find('\n').map(|i| start + i).unwrap_or(s.len());
        let line = &s[start..line_end];
        let stripped = line.trim_end_matches('\r');
        if stripped == "---" {
            return Some(start);
        }
        start = line_end + 1;
    }
    None
}

/// Best-effort YAML tag extraction. Recognizes:
///   tags: [a, b, c]
///   tags:
///     - a
///     - b
///   tags: a
fn extract_tags(frontmatter: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut lines = frontmatter.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("tags:") else { continue };
        let rest = rest.trim();
        if rest.starts_with('[') && rest.ends_with(']') {
            // inline list
            let inner = &rest[1..rest.len() - 1];
            for t in inner.split(',') {
                let cleaned = clean_tag(t);
                if !cleaned.is_empty() {
                    tags.push(cleaned);
                }
            }
        } else if !rest.is_empty() {
            // single value
            let cleaned = clean_tag(rest);
            if !cleaned.is_empty() {
                tags.push(cleaned);
            }
        } else {
            // block list — peek lines that start with whitespace + `-`
            while let Some(&peek) = lines.peek() {
                let p = peek;
                let pt = p.trim_start();
                if let Some(item) = pt.strip_prefix("- ") {
                    let cleaned = clean_tag(item);
                    if !cleaned.is_empty() {
                        tags.push(cleaned);
                    }
                    lines.next();
                } else if pt.is_empty() {
                    lines.next();
                } else if !p.starts_with(char::is_whitespace) {
                    break;
                } else {
                    break;
                }
            }
        }
        // Only the first `tags:` block matters.
        break;
    }
    tags
}

fn clean_tag(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('#')
        .to_string()
}

fn first_h1(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") {
            let title = rest.trim().trim_end_matches('#').trim().to_string();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_strip_basic() {
        let raw = "---\ntitle: foo\ntags: [a, b]\n---\nbody here\n";
        let (fm, body) = split_frontmatter(raw);
        assert!(fm.contains("title: foo"));
        assert_eq!(body, "body here\n");
    }

    #[test]
    fn frontmatter_no_fence_means_no_fm() {
        let raw = "no frontmatter\n# Title\nbody\n";
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm, "");
        assert_eq!(body, raw);
    }

    #[test]
    fn extract_tags_inline() {
        let fm = "title: x\ntags: [foo, bar, baz]\n";
        assert_eq!(extract_tags(fm), vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn extract_tags_block() {
        let fm = "tags:\n  - foo\n  - bar\n";
        assert_eq!(extract_tags(fm), vec!["foo", "bar"]);
    }

    #[test]
    fn first_h1_works() {
        assert_eq!(first_h1("# Hello\nbody"), Some("Hello".to_string()));
        assert_eq!(first_h1("body only"), None);
    }
}
