//! Bounded, traversal-safe tar.gz extraction for codeload tarballs.
//!
//! codeload wraps every entry in a single top-level `{repo}-{ref}/`
//! directory; extraction strips that component so the destination mirrors the
//! repository root. Only regular files and directories are materialized —
//! symlinks, hardlinks, and device entries are skipped — and both the
//! per-archive path set and the cumulative byte count are strictly validated
//! so a hostile or corrupt tarball cannot write outside `dest` or exhaust
//! disk.

use std::path::{Component, Path, PathBuf};

use data_loader::ImportError;
use flate2::read::GzDecoder;

/// Extract a gzipped codeload tarball into `dest` (which must already exist),
/// stripping the single top-level directory and rejecting the archive when
/// the cumulative extracted size exceeds `max_bytes`.
pub fn extract_tarball(bytes: &[u8], dest: &Path, max_bytes: u64) -> Result<(), ImportError> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut total: u64 = 0;
    let entries = archive.entries().map_err(|error| corrupt(error))?;
    for entry in entries {
        let mut entry = entry.map_err(corrupt)?;
        let path = entry.path().map_err(corrupt)?.into_owned();
        let Some(relative) = strip_top_level(&path)? else {
            continue;
        };
        let target = dest.join(&relative);
        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::Directory {
            std::fs::create_dir_all(&target).map_err(|error| ImportError::SourceRead {
                origin: target.display().to_string(),
                message: format!("create extracted directory: {error}"),
            })?;
            continue;
        }
        if !entry_type.is_file() {
            // Symlinks/hardlinks/devices: not part of a vault corpus and a
            // classic extraction-escape vector.
            continue;
        }
        let size = entry.header().size().map_err(corrupt)?;
        total = total.saturating_add(size);
        if total > max_bytes {
            return Err(ImportError::SourceRead {
                origin: "codeload tarball".into(),
                message: format!("extracted contents exceed the {max_bytes} byte bound"),
            });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| ImportError::SourceRead {
                origin: parent.display().to_string(),
                message: format!("create extracted directory: {error}"),
            })?;
        }
        let mut file = std::fs::File::create(&target).map_err(|error| ImportError::SourceRead {
            origin: target.display().to_string(),
            message: format!("create extracted file: {error}"),
        })?;
        std::io::copy(&mut entry, &mut file).map_err(|error| ImportError::SourceRead {
            origin: target.display().to_string(),
            message: format!("write extracted file: {error}"),
        })?;
    }
    Ok(())
}

/// Strip the top-level `{repo}-{ref}/` component and prove the remaining
/// path is a safe relative path (all `Normal` components — no root, prefix,
/// or `..`). Returns `None` for the top-level directory entry itself.
fn strip_top_level(path: &Path) -> Result<Option<PathBuf>, ImportError> {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(_)) | Some(Component::CurDir) => {}
        // A rooted or parent-leading entry is never legitimate in a tarball.
        _ => {
            return Err(ImportError::SourceRead {
                origin: "codeload tarball".into(),
                message: format!("unsafe archive path {}", path.display()),
            })
        }
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => {
                return Err(ImportError::SourceRead {
                    origin: "codeload tarball".into(),
                    message: format!("unsafe archive path {}", path.display()),
                })
            }
        }
    }
    if relative.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(relative))
    }
}

fn corrupt(error: std::io::Error) -> ImportError {
    ImportError::SourceRead {
        origin: "codeload tarball".into(),
        message: format!("corrupt tarball: {error}"),
    }
}
