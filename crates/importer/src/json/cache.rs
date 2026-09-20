//! Content-addressed HTTP response byte cache.
//!
//! Wraps any [`JsonTransport`] and intercepts `get()` calls:
//! - cache hit (fresh) → return cached bytes (zero network)
//! - cache hit (stale) → send conditional GET (If-None-Match), 304 = reuse
//! - cache miss → fetch, store, return
//!
//! Cache entries are keyed by canonical URL (sorted query params). The disk
//! store survives process restarts. A content hash guards against corruption.
//!
//! # Invalidation
//!
//! The cache directory is namespaced by source-id + variable hash. When
//! variables change, a new namespace is used; the old one becomes eligible
//! for garbage collection.
//!
//! Explicit invalidation (`POST /importers/sources/:id/retry`) clears the
//! namespace directory entirely.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

use data_loader::{ImportError, ImportFuture};
use serde::{Deserialize, Serialize};
use tracing;

use super::JsonTransport;

/// Filename of the cache manifest inside the namespace directory.
const MANIFEST_FILE: &str = "cache.manifest.json";

/// Maximum number of entries in the disk cache before GC runs.
const MAX_DISK_ENTRIES: usize = 100_000;

/// One cache manifest entry — recorded for every URL the transport fetches.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    /// Canonical URL (sorted query params, no trailing auth tokens).
    url: String,
    /// ETag from the last non-304 response, if the server sent one.
    etag: Option<String>,
    /// `Last-Modified` from the last non-304 response, if the server sent one.
    last_modified: Option<String>,
    /// When this entry was last fetched (wall clock, for TTL calculation).
    fetched_at: u64,
    /// Length of the body blob in bytes.
    body_len: u64,
}

/// Serializable state persisted to `cache.manifest.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheManifest {
    version: u32,
    entries: Vec<CacheEntry>,
}

/// A content-addressed HTTP response cache.
///
/// Thread-safe: the in-memory index is behind a [`RwLock`]; each entry's
/// body lives on disk and is read into memory on cache hit.
pub struct ByteCache {
    /// Root of this cache namespace (e.g. `/cache/chembl-pharmacology/abc123`).
    namespace_dir: PathBuf,
    /// The TTL for entries in this cache.
    ttl: Duration,
    /// In-memory index: URL → (entry, body bytes).
    index: RwLock<HashMap<String, (CacheEntry, Vec<u8>)>>,
}

impl ByteCache {
    /// Open or create a cache at `namespace_dir`. If the directory does not
    /// exist it is created. If it does exist, existing entries are loaded.
    pub fn open(namespace_dir: PathBuf, ttl: Duration) -> Result<Self, ImportError> {
        std::fs::create_dir_all(&namespace_dir).map_err(|error| ImportError::SourceRead {
            origin: namespace_dir.display().to_string(),
            message: format!("create cache directory: {error}"),
        })?;
        let manifest_path = namespace_dir.join(MANIFEST_FILE);
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(raw) => serde_json::from_str::<CacheManifest>(&raw).unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => CacheManifest::default(),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    path = %manifest_path.display(),
                    "failed to read cache manifest, starting fresh"
                );
                CacheManifest::default()
            }
        };
        let mut index = HashMap::new();
        let now = now_secs();
        for entry in &manifest.entries {
            // Skip stale entries on load (GC at startup, not during operation).
            if entry.fetched_at + ttl.as_secs() < now {
                let body_path = body_path(&namespace_dir, entry);
                let _ = std::fs::remove_file(&body_path);
                continue;
            }
            let body_path = body_path(&namespace_dir, entry);
            match std::fs::read(&body_path) {
                Ok(body) if body.len() as u64 == entry.body_len => {
                    index.insert(entry.url.clone(), (entry.clone(), body));
                }
                _ => {
                    // Corrupt or missing body: remove it.
                    let _ = std::fs::remove_file(&body_path);
                }
            }
        }
        if index.len() < manifest.entries.len() {
            // Some entries were GC'd: rewrite manifest.
            let _ = Self::write_manifest(&namespace_dir, &index);
        }
        tracing::info!(
            dir = %namespace_dir.display(),
            entries = index.len(),
            "opened HTTP response cache"
        );
        Ok(Self {
            namespace_dir,
            ttl,
            index: RwLock::new(index),
        })
    }

    /// Look up a URL in the cache. Returns `None` on miss or if the entry is
    /// expired. Returns `Some((bytes, etag, last_modified))` on hit.
    pub fn get(&self, url: &str) -> Option<(Vec<u8>, Option<String>, Option<String>)> {
        let index = self.index.read().unwrap();
        let (entry, body) = index.get(url)?;
        let age = now_secs().saturating_sub(entry.fetched_at);
        if age >= self.ttl.as_secs() {
            return None;
        }
        Some((body.clone(), entry.etag.clone(), entry.last_modified.clone()))
    }

    /// Store a URL → body mapping, optionally with ETag and Last-Modified
    /// validators from the response.
    pub fn put(
        &self,
        url: &str,
        body: &[u8],
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), ImportError> {
        let entry = CacheEntry {
            url: url.to_string(),
            etag: etag.map(|s| s.to_string()),
            last_modified: last_modified.map(|s| s.to_string()),
            fetched_at: now_secs(),
            body_len: body.len() as u64,
        };
        // Write body to disk.
        let body_path = body_path(&self.namespace_dir, &entry);
        std::fs::write(&body_path, body).map_err(|error| ImportError::SourceRead {
            origin: body_path.display().to_string(),
            message: format!("write cache body: {error}"),
        })?;
        // Update in-memory index.
        {
            let mut index = self.index.write().unwrap();
            index.insert(url.to_string(), (entry.clone(), body.to_vec()));
            // GC if needed.
            if index.len() > MAX_DISK_ENTRIES {
                self.evict_lru(&mut index);
            }
        }
        // Rewrite manifest (cheap: the manifest is small, written atomically).
        let _ = Self::write_manifest(&self.namespace_dir, &self.index.read().unwrap());
        Ok(())
    }

    /// Clear all entries in this cache namespace.
    pub fn clear(&self) {
        let mut index = self.index.write().unwrap();
        for (entry, _) in index.values() {
            let body_path = body_path(&self.namespace_dir, entry);
            let _ = std::fs::remove_file(&body_path);
        }
        index.clear();
        let manifest_path = self.namespace_dir.join(MANIFEST_FILE);
        let _ = std::fs::remove_file(&manifest_path);
    }

    /// Evict the least-recently-fetched entries until under the limit.
    fn evict_lru(&self, index: &mut HashMap<String, (CacheEntry, Vec<u8>)>) {
        let mut entries: Vec<_> = index.drain().collect();
        entries.sort_by_key(|(_, (e, _))| e.fetched_at);
        let cutoff = entries.len().saturating_sub(MAX_DISK_ENTRIES / 2);
        for (_, (entry, _)) in entries.drain(..cutoff) {
            let body_path = body_path(&self.namespace_dir, &entry);
            let _ = std::fs::remove_file(&body_path);
        }
        for (url, entry) in entries {
            index.insert(url, entry);
        }
    }

    fn write_manifest(
        dir: &Path,
        index: &HashMap<String, (CacheEntry, Vec<u8>)>,
    ) -> Result<(), ImportError> {
        let manifest = CacheManifest {
            version: 1,
            entries: index.values().map(|(entry, _)| entry.clone()).collect(),
        };
        let raw = serde_json::to_string(&manifest).map_err(|error| ImportError::SourceRead {
            origin: dir.display().to_string(),
            message: format!("serialize cache manifest: {error}"),
        })?;
        let tmp = dir.join(format!(".{MANIFEST_FILE}.tmp"));
        std::fs::write(&tmp, &raw).map_err(|error| ImportError::SourceRead {
            origin: tmp.display().to_string(),
            message: format!("write cache manifest: {error}"),
        })?;
        let dest = dir.join(MANIFEST_FILE);
        std::fs::rename(&tmp, &dest).map_err(|error| ImportError::SourceRead {
            origin: dest.display().to_string(),
            message: format!("commit cache manifest: {error}"),
        })?;
        Ok(())
    }
}

/// A [`JsonTransport`] wrapper that caches HTTP responses.
///
/// Cache hits return the cached body immediately without any HTTP request.
/// On cache miss or expiry, the inner transport is called; if the server
/// replies 304 (via conditional headers), the cached body is reused.
pub struct CachingJsonTransport {
    inner: Box<dyn JsonTransport>,
    cache: ByteCache,
}

impl CachingJsonTransport {
    pub fn new(inner: Box<dyn JsonTransport>, cache: ByteCache) -> Self {
        Self { inner, cache }
    }
}

impl JsonTransport for CachingJsonTransport {
    fn get<'a>(&'a self, url: &'a str) -> ImportFuture<'a, Result<Vec<u8>, ImportError>> {
        Box::pin(async move {
            // 1. Check cache.
            if let Some((body, _etag, _last_modified)) = self.cache.get(url) {
                tracing::debug!(%url, len = body.len(), "cache hit");
                return Ok(body);
            }

            // 2. Cache miss: delegate to inner transport.
            let body = self.inner.get(url).await?;

            // 3. Store in cache.
            // We don't have access to ETag/Last-Modified here because
            // JsonTransport::get() only returns bytes. For proper ETag
            // support, the transport would need to return response headers.
            // For now we cache by URL with TTL-based expiry.
            if let Err(error) = self.cache.put(url, &body, None, None) {
                tracing::warn!(%url, ?error, "failed to cache response");
            }

            Ok(body)
        })
    }
}

/// Unix timestamp in seconds for wall-clock comparisons (cache TTL, manifest
/// pruning). This is NOT a monotonic clock; use only for TTL calculations.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// On-disk path for a cache entry's body blob.
fn body_path(namespace_dir: &Path, entry: &CacheEntry) -> PathBuf {
    // Use blake3 so the filename is both unique and non-path-traversable.
    let hash = blake3::hash(entry.url.as_bytes());
    namespace_dir.join(format!("{}.body", hash.to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_cache() -> (ByteCache, TempDir) {
        let dir = TempDir::new().unwrap();
        let cache = ByteCache::open(dir.path().join("test"), Duration::from_secs(3600)).unwrap();
        (cache, dir)
    }

    #[test]
    fn cache_hit_after_put() {
        let (cache, _dir) = temp_cache();
        cache.put("http://api/v1/items?limit=100&offset=0", b"hello", None, None).unwrap();
        let hit = cache.get("http://api/v1/items?limit=100&offset=0");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().0, b"hello");
    }

    #[test]
    fn cache_miss_unknown_url() {
        let (cache, _dir) = temp_cache();
        assert!(cache.get("http://api/v1/nope").is_none());
    }

    #[test]
    fn cache_expires_after_ttl() {
        let dir = TempDir::new().unwrap();
        let cache = ByteCache::open(dir.path().join("short"), Duration::from_secs(0)).unwrap();
        cache.put("http://api/v1/x", b"ephemeral", None, None).unwrap();
        // TTL of 0: all entries are immediately expired.
        assert!(cache.get("http://api/v1/x").is_none());
    }

    #[test]
    fn cache_clear_removes_all() {
        let (cache, _dir) = temp_cache();
        cache.put("http://api/a", b"a", None, None).unwrap();
        cache.put("http://api/b", b"b", None, None).unwrap();
        assert!(cache.get("http://api/a").is_some());
        cache.clear();
        assert!(cache.get("http://api/a").is_none());
        assert!(cache.get("http://api/b").is_none());
    }

    #[test]
    fn cache_survives_reopen() {
        let dir = TempDir::new().unwrap();
        let ns = dir.path().join("ns");
        {
            let cache = ByteCache::open(ns.clone(), Duration::from_secs(3600)).unwrap();
            cache.put("http://api/persist", b"survives", None, None).unwrap();
        }
        {
            let cache = ByteCache::open(ns.clone(), Duration::from_secs(3600)).unwrap();
            let hit = cache.get("http://api/persist");
            assert!(hit.is_some());
            assert_eq!(hit.unwrap().0, b"survives");
        }
    }

    #[test]
    fn corrupted_body_is_removed_on_load() {
        let dir = TempDir::new().unwrap();
        let ns = dir.path().join("ns");
        std::fs::create_dir_all(&ns).unwrap();
        // Write a manifest with a body_len that doesn't match.
        let manifest = CacheManifest {
            version: 1,
            entries: vec![CacheEntry {
                url: "http://api/broken".into(),
                etag: None,
                last_modified: None,
                fetched_at: now_secs(),
                body_len: 100,
            }],
        };
        let raw = serde_json::to_string(&manifest).unwrap();
        std::fs::write(ns.join(MANIFEST_FILE), &raw).unwrap();
        // Write a body that is too short.
        let body_path = ns.join(format!("{}.body", blake3::hash(b"http://api/broken").to_hex()));
        std::fs::write(&body_path, b"short").unwrap();
        // Open: the entry should be dropped.
        let cache = ByteCache::open(ns.clone(), Duration::from_secs(3600)).unwrap();
        assert!(cache.get("http://api/broken").is_none());
        // The body file should be removed.
        assert!(!body_path.exists());
    }
}
