use anyhow::{Context, Result};
use ignore::{overrides::OverrideBuilder, WalkBuilder};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directories and patterns excluded per the canonical contract shared with
/// vault-search. Uses `ignore` override globs with `**/` prefixes so exclusions
/// apply at any depth, not just the vault root.
const EXCLUDES: &[&str] = &[
    "!**/.obsidian/**",
    "!**/.git/**",
    "!**/.jj/**",
    "!**/Excalidraw/**",
    "!**/Ink/**",
    "!**/_hippo/**",
    "!**/*.base",
    "!**/*.canvas",
];

/// Build an `ignore::Walk` honoring the canonical exclusion contract.
pub fn build_walker(root: &Path) -> Result<ignore::Walk> {
    let mut overrides = OverrideBuilder::new(root);
    for pat in EXCLUDES {
        overrides.add(pat).with_context(|| format!("override pat: {pat}"))?;
    }
    let overrides = overrides.build().context("build overrides")?;

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .overrides(overrides)
        .build();

    Ok(walker)
}

/// Walk the vault and return `(rel_id, abs_path, mtime_secs)` for every `.md`
/// file that survives the exclusion contract.
pub fn list_markdown(root: &Path) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut out = Vec::new();
    for entry in build_walker(root)? {
        let entry: ignore::DirEntry = match entry {
            Ok(e) => e,
            Err(err) => {
                eprintln!("walk error: {err}");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s: &std::ffi::OsStr| s.to_str()) != Some("md") {
            continue;
        }
        let rel: &std::path::Path = match path.strip_prefix(root) {
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
            .and_then(|m: std::fs::Metadata| m.modified())
            .map(systime_to_secs)
            .unwrap_or(0);
        out.push((id, path.to_path_buf(), mtime));
    }
    Ok(out)
}

/// Strictly walk the vault for importer publication.
///
/// Unlike [`list_markdown`], this path rejects walk and metadata failures so a
/// reload cannot publish a partial graph when part of the source is unreadable.
pub fn try_list_markdown(root: &Path) -> Result<Vec<(String, PathBuf, u64)>> {
    let root_metadata = root
        .metadata()
        .with_context(|| format!("read vault root metadata for {}", root.display()))?;
    anyhow::ensure!(
        root_metadata.is_dir(),
        "vault root {} is not a directory",
        root.display()
    );
    let mut out = Vec::new();
    for entry in build_walker(root)? {
        let entry = entry.with_context(|| format!("walk vault {}", root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let metadata = path
            .metadata()
            .with_context(|| format!("read metadata for {}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).with_context(|| {
            format!(
                "markdown path {} is outside vault {}",
                path.display(),
                root.display()
            )
        })?;
        let id = rel
            .with_extension("")
            .to_string_lossy()
            .to_string()
            .replace('\\', "/");
        let mtime = metadata
            .modified()
            .with_context(|| format!("read modified time for {}", path.display()))
            .map(systime_to_secs)?;
        out.push((id, path.to_path_buf(), mtime));
    }
    Ok(out)
}

fn systime_to_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
