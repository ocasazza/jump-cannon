//! Archive envelope expansion — pure Rust, wasm-clean.
//!
//! [`expand_envelope`] detects tar, tar.gz, zip, and gzip payloads by magic
//! bytes and emits one [`SourceRecord`] per contained file with the entry
//! path in record metadata. A payload that matches no archive signature
//! passes through unchanged. [`EnvelopeConnector`] applies the same expansion
//! as a decorator over any inner [`SourceConnector`], so every transport gets
//! archive handling for free.
//!
//! Bounds are enforced during expansion, not after: each archive entry is
//! size-checked from its header before a byte is decompressed, and running
//! totals stop extraction at the configured entry/byte caps.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, ImportProgress, SourceConnector, SourceRecord,
};

use crate::guess_content_type;

/// Metadata key recording the origin of the outermost container the record
/// was expanded from.
pub const CONTAINER_KEY: &str = "envelope.container";

/// Metadata key recording the entry path within the container. Nested
/// archives chain with `!`: `dir/inner.zip!notes.md` names `notes.md` inside
/// the `dir/inner.zip` entry of the fetched container.
pub const ENTRY_KEY: &str = "envelope.entry";

/// Default entry-count bound.
pub const DEFAULT_MAX_ENTRIES: usize = 1024;
/// Default per-entry (decompressed) byte bound: 64 MiB.
pub const DEFAULT_MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
/// Default total (decompressed) byte bound across all entries: 256 MiB.
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// Bound on nested archive depth (`a.zip` containing `b.gz` containing …).
const MAX_DEPTH: usize = 8;

/// Extraction bounds for [`expand_envelope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeLimits {
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for EnvelopeLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// Expand one fetched record: archive payloads become one record per entry,
/// anything else passes through unchanged.
pub fn expand_envelope(
    record: SourceRecord,
    limits: &EnvelopeLimits,
) -> Result<Vec<SourceRecord>, ImportError> {
    if matches!(detect(&record.bytes), Kind::Plain) {
        return Ok(vec![record]);
    }
    let root = record.origin.clone();
    let fallback = root.rsplit('/').next().unwrap_or(&root).to_string();
    let mut budget = Budget::new(limits);
    let mut out = Vec::new();
    expand_bytes(&root, None, &fallback, record.bytes, 0, limits, &mut budget, &mut out)?;
    Ok(out)
}

/// Source connector decorator: reads from the inner connector, then expands
/// every archive record into one record per entry. Capabilities delegate to
/// the inner connector — expansion performs no I/O of its own.
pub struct EnvelopeConnector {
    inner: Box<dyn SourceConnector>,
    limits: EnvelopeLimits,
}

impl EnvelopeConnector {
    pub fn new(inner: Box<dyn SourceConnector>, limits: EnvelopeLimits) -> Self {
        Self { inner, limits }
    }

    pub fn with_default_limits(inner: Box<dyn SourceConnector>) -> Self {
        Self::new(inner, EnvelopeLimits::default())
    }
}

impl SourceConnector for EnvelopeConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        self.inner.capabilities(effect)
    }

    fn read<'a>(
        &'a self,
        progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move {
            let mut records = Vec::new();
            for record in self.inner.read(progress).await? {
                records.extend(expand_envelope(record, &self.limits)?);
            }
            Ok(records)
        })
    }

    fn write<'a>(
        &'a self,
        request: data_loader::WriteRequest,
    ) -> ImportFuture<'a, Result<data_loader::WriteReceipt, ImportError>> {
        self.inner.write(request)
    }
}

enum Kind {
    Zip,
    Gzip,
    Tar,
    Plain,
}

/// Detect the archive format by magic bytes, never by file name: zip local
/// header (or empty-archive end record), gzip member header, then the tar
/// `ustar` magic at offset 257.
fn detect(bytes: &[u8]) -> Kind {
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        Kind::Zip
    } else if bytes.starts_with(b"\x1f\x8b") {
        Kind::Gzip
    } else if bytes.len() >= 512 && &bytes[257..262] == b"ustar" {
        Kind::Tar
    } else {
        Kind::Plain
    }
}

struct Budget {
    remaining_entries: usize,
    remaining_bytes: usize,
    max_entry_bytes: usize,
}

impl Budget {
    fn new(limits: &EnvelopeLimits) -> Self {
        Self {
            remaining_entries: limits.max_entries,
            remaining_bytes: limits.max_total_bytes,
            max_entry_bytes: limits.max_entry_bytes,
        }
    }

    fn take_entry(&mut self, origin: &str) -> Result<(), ImportError> {
        if self.remaining_entries == 0 {
            return Err(ImportError::SourceRead {
                origin: origin.to_string(),
                message: "envelope entry-count bound reached".to_string(),
            });
        }
        self.remaining_entries -= 1;
        Ok(())
    }

    fn take_bytes(&mut self, origin: &str, bytes: usize) -> Result<(), ImportError> {
        if bytes > self.remaining_bytes {
            return Err(ImportError::SourceRead {
                origin: origin.to_string(),
                message: "envelope total-byte bound reached".to_string(),
            });
        }
        self.remaining_bytes -= bytes;
        Ok(())
    }
}

/// Expand one payload. `chain` is the cumulative entry path from the
/// container root (`None` while still at the fetched root); `fallback` names
/// the payload when a bare gzip member carries no path of its own.
fn expand_bytes(
    root: &str,
    chain: Option<String>,
    fallback: &str,
    bytes: Vec<u8>,
    depth: usize,
    limits: &EnvelopeLimits,
    budget: &mut Budget,
    out: &mut Vec<SourceRecord>,
) -> Result<(), ImportError> {
    if depth > MAX_DEPTH {
        return Err(ImportError::SourceRead {
            origin: root.to_string(),
            message: format!("envelope nesting exceeds the {MAX_DEPTH} level bound"),
        });
    }
    match detect(&bytes) {
        Kind::Plain => {
            let entry = chain.unwrap_or_else(|| fallback.to_string());
            out.push(leaf_record(root, entry, bytes));
            Ok(())
        }
        Kind::Gzip => {
            let origin = chain
                .as_deref()
                .map(|entry| format!("{root}!{entry}"))
                .unwrap_or_else(|| root.to_string());
            let decompressed = read_capped(
                flate2::read::GzDecoder::new(Cursor::new(bytes)),
                limits.max_entry_bytes,
                &origin,
            )?;
            budget.take_bytes(&origin, decompressed.len())?;
            // gzip wraps a single file: strip one `.gz` suffix from the name
            // the decompressed payload will carry if it turns out to be
            // plain (archives inside supply their own entry paths).
            let (chain, fallback) = match chain {
                Some(chain) => (Some(strip_gz_suffix(&chain)), fallback.to_string()),
                None => (None, strip_gz_suffix(fallback)),
            };
            expand_bytes(root, chain, &fallback, decompressed, depth + 1, limits, budget, out)
        }
        Kind::Zip => {
            let origin = chain
                .as_deref()
                .map(|entry| format!("{root}!{entry}"))
                .unwrap_or_else(|| root.to_string());
            let mut archive =
                zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!("invalid zip envelope: {error}"),
                })?;
            for index in 0..archive.len() {
                let mut entry =
                    archive
                        .by_index(index)
                        .map_err(|error| ImportError::SourceRead {
                            origin: origin.clone(),
                            message: format!("zip entry {index} unreadable: {error}"),
                        })?;
                // `enclosed_name` rejects path-traversal entries.
                let Some(path) = entry.enclosed_name() else {
                    continue;
                };
                if entry.is_dir() {
                    continue;
                }
                let path = path.to_string_lossy().replace('\\', "/");
                let entry_origin = format!("{origin}!{path}");
                budget.take_entry(&entry_origin)?;
                if entry.size() as usize > budget.max_entry_bytes {
                    return Err(ImportError::SourceRead {
                        origin: entry_origin,
                        message: format!(
                            "zip entry declares {} bytes, exceeding the {} byte bound",
                            entry.size(),
                            budget.max_entry_bytes
                        ),
                    });
                }
                let payload = read_capped(&mut entry, budget.max_entry_bytes, &entry_origin)?;
                budget.take_bytes(&entry_origin, payload.len())?;
                let child_chain = Some(match &chain {
                    Some(parent) => format!("{parent}!{path}"),
                    None => path,
                });
                expand_bytes(
                    root,
                    child_chain,
                    fallback,
                    payload,
                    depth + 1,
                    limits,
                    budget,
                    out,
                )?;
            }
            Ok(())
        }
        Kind::Tar => {
            let origin = chain
                .as_deref()
                .map(|entry| format!("{root}!{entry}"))
                .unwrap_or_else(|| root.to_string());
            let mut archive = tar::Archive::new(Cursor::new(bytes));
            let entries = archive
                .entries()
                .map_err(|error| ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!("invalid tar envelope: {error}"),
                })?;
            for entry in entries {
                let mut entry = entry.map_err(|error| ImportError::SourceRead {
                    origin: origin.clone(),
                    message: format!("tar entry unreadable: {error}"),
                })?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry
                    .path()
                    .map_err(|error| ImportError::SourceRead {
                        origin: origin.clone(),
                        message: format!("tar entry path unreadable: {error}"),
                    })?
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.split('/').any(|part| part == ".." || part.is_empty()) {
                    continue;
                }
                let entry_origin = format!("{origin}!{path}");
                budget.take_entry(&entry_origin)?;
                let declared = entry.header().size().unwrap_or(u64::MAX) as usize;
                if declared > budget.max_entry_bytes {
                    return Err(ImportError::SourceRead {
                        origin: entry_origin,
                        message: format!(
                            "tar entry declares {declared} bytes, exceeding the {} byte bound",
                            budget.max_entry_bytes
                        ),
                    });
                }
                let payload = read_capped(&mut entry, budget.max_entry_bytes, &entry_origin)?;
                budget.take_bytes(&entry_origin, payload.len())?;
                let child_chain = Some(match &chain {
                    Some(parent) => format!("{parent}!{path}"),
                    None => path,
                });
                expand_bytes(
                    root,
                    child_chain,
                    fallback,
                    payload,
                    depth + 1,
                    limits,
                    budget,
                    out,
                )?;
            }
            Ok(())
        }
    }
}

fn strip_gz_suffix(name: &str) -> String {
    match name.rsplit_once('/') {
        Some((dir, file)) => format!(
            "{dir}/{}",
            file.strip_suffix(".gz").unwrap_or(file)
        ),
        None => name.strip_suffix(".gz").unwrap_or(name).to_string(),
    }
}

fn leaf_record(root: &str, entry: String, bytes: Vec<u8>) -> SourceRecord {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        CONTAINER_KEY.to_string(),
        serde_json::Value::String(root.to_string()),
    );
    metadata.insert(
        ENTRY_KEY.to_string(),
        serde_json::Value::String(entry.clone()),
    );
    SourceRecord {
        origin: format!("{root}!{entry}"),
        content_type: guess_content_type(&entry).to_string(),
        bytes,
        metadata,
    }
}

fn read_capped<R: Read>(
    mut reader: R,
    limit: usize,
    origin: &str,
) -> Result<Vec<u8>, ImportError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 32 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| ImportError::SourceRead {
                origin: origin.to_string(),
                message: format!("envelope read failed: {error}"),
            })?;
        if read == 0 {
            break;
        }
        if bytes.len() + read > limit {
            return Err(ImportError::SourceRead {
                origin: origin.to_string(),
                message: format!("entry exceeds the {limit} byte bound"),
            });
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
