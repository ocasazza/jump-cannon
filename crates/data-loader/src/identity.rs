//! Unified node identity and content addressing for every importer.
//!
//! Every graph node ID published by an importer is exactly
//!
//! ```text
//! NodeId = "{source_kind}:{source_id}:{local}"
//! ```
//!
//! - `source_kind`: the lowercase [`crate::SourceKind`] identifier
//!   (`obsidian`, `tvix`, `generate`, `kubernetes`, `okf`, `pest`, …),
//!   validated as `[a-z0-9]{1,32}`.
//! - `source_id`: the per-node `meta.source_id`, validated as
//!   `[a-z0-9._-]{1,128}` (no `:` — it is a namespace segment).
//! - `local`: the importer's natural key — non-empty, at most 512 bytes, and
//!   may contain `/` and `:` (vault-relative paths, `uid:{uid}`, …).
//!
//! When a source has no natural key at all, the local part falls back to a
//! content address: `h256:{32hex}` from [`Namespace::content_id`].
//!
//! # Content canonicalization
//!
//! [`Namespace::content_id`] hashes **byte-exact content only** — no mtime,
//! no path, no metadata, and no normalization. When the content is a
//! structured record rather than a byte string, THE fallback serialization is
//! a u32-LE length-prefixed concatenation of the record's fields in
//! lexicographic key order (`u32-LE len ‖ bytes` per field). Importers that
//! need the `h256:` fallback for structured captures must serialize with that
//! rule so equal records hash identically across processes.

use sha2::{Digest, Sha256};

use crate::ImportError;

/// Maximum bytes in a `source_kind` segment (`[a-z0-9]{1,32}`).
pub const MAX_SOURCE_KIND_BYTES: usize = 32;
/// Maximum bytes in a `source_id` segment (`[a-z0-9._-]{1,128}`, no `:`).
pub const MAX_SOURCE_ID_BYTES: usize = 128;
/// Maximum bytes in the local segment of a node ID.
pub const MAX_LOCAL_ID_BYTES: usize = 512;

/// Validate a `source_kind` segment: `[a-z0-9]{1,32}`.
pub fn validate_source_kind(kind: &str) -> Result<(), String> {
    let valid = !kind.is_empty()
        && kind.len() <= MAX_SOURCE_KIND_BYTES
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(format!(
            "source_kind must be 1..={MAX_SOURCE_KIND_BYTES} bytes of [a-z0-9], got {kind:?}"
        ))
    }
}

/// Validate a `source_id` segment: `[a-z0-9._-]{1,128}` (no `:`).
///
/// This is the shared form of the rule every importer applies to its
/// configured source instance identifier so `source_id` can never make node
/// namespaces ambiguous.
pub fn validate_source_id(source_id: &str) -> Result<(), String> {
    let valid = !source_id.is_empty()
        && source_id.len() <= MAX_SOURCE_ID_BYTES
        && source_id.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "source_id must be 1..={MAX_SOURCE_ID_BYTES} bytes of [a-z0-9._-], got {source_id:?}"
        ))
    }
}

/// Validate the local segment of a node ID: non-empty, at most
/// [`MAX_LOCAL_ID_BYTES`] bytes. `/` and `:` are permitted.
pub fn validate_local_id(local: &str) -> Result<(), String> {
    if local.is_empty() {
        return Err("local id must be non-empty".into());
    }
    if local.len() > MAX_LOCAL_ID_BYTES {
        return Err(format!(
            "local id exceeds {MAX_LOCAL_ID_BYTES} bytes ({} bytes)",
            local.len()
        ));
    }
    Ok(())
}

/// A validated `{source_kind}:{source_id}:` node-ID namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    kind: String,
    source_id: String,
    prefix: String,
}

impl Namespace {
    /// Build the namespace for one importer source instance. Violations are
    /// mapping errors: an importer with an invalid identity configuration can
    /// never publish a graph.
    pub fn new(kind: &str, source_id: &str) -> Result<Self, ImportError> {
        validate_source_kind(kind).map_err(|message| ImportError::Map { message })?;
        validate_source_id(source_id).map_err(|message| ImportError::Map { message })?;
        Ok(Self {
            kind: kind.to_string(),
            source_id: source_id.to_string(),
            prefix: format!("{kind}:{source_id}:"),
        })
    }

    /// The validated `source_kind` segment.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The validated `source_id` segment.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The `{source_kind}:{source_id}:` prefix every node ID starts with.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The canonical node ID for one local key.
    pub fn node_id(&self, local: &str) -> Result<String, ImportError> {
        validate_local_id(local).map_err(|message| ImportError::Map { message })?;
        Ok(format!("{}{local}", self.prefix))
    }

    /// Content address of `bytes`: `h256:{32hex}` — SHA-256 truncated to 16
    /// bytes, lowercase hex. Used as the local-ID fallback when a source has
    /// no natural key, and as the `content_hash` discovery value for sources
    /// with byte-addressable content. See the module docs for THE structured
    /// record serialization used before hashing.
    pub fn content_id(&self, bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        format!("h256:{}", hex_lower(&digest[..16]))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_builds_canonical_node_ids() {
        let namespace = Namespace::new("obsidian", "vault-a").unwrap();
        assert_eq!(namespace.kind(), "obsidian");
        assert_eq!(namespace.source_id(), "vault-a");
        assert_eq!(namespace.prefix(), "obsidian:vault-a:");
        assert_eq!(
            namespace.node_id("notes/todo").unwrap(),
            "obsidian:vault-a:notes/todo"
        );
        // ':' and '/' are legal inside the local segment.
        assert_eq!(
            namespace.node_id("uid:abc-123").unwrap(),
            "obsidian:vault-a:uid:abc-123"
        );
    }

    #[test]
    fn namespace_rejects_invalid_kinds_and_source_ids() {
        for kind in ["", "UPPER", "has space", "colon:x", "slash/x"] {
            assert!(Namespace::new(kind, "fixture").is_err(), "{kind}");
        }
        assert!(Namespace::new(&"a".repeat(33), "fixture").is_err());
        for source_id in ["", "has space", "a:b", "path/segment", "unicode-λ", "UPPER"] {
            assert!(Namespace::new("okf", source_id).is_err(), "{source_id}");
        }
        assert!(Namespace::new("okf", &"a".repeat(129)).is_err());
        assert!(Namespace::new("okf", "team-blue_2.prod").is_ok());
    }

    #[test]
    fn node_id_rejects_empty_and_oversized_locals() {
        let namespace = Namespace::new("okf", "fixture").unwrap();
        assert!(namespace.node_id("").is_err());
        assert!(namespace.node_id(&"x".repeat(MAX_LOCAL_ID_BYTES)).is_ok());
        assert!(namespace.node_id(&"x".repeat(MAX_LOCAL_ID_BYTES + 1)).is_err());
    }

    #[test]
    fn content_id_is_a_stable_truncated_sha256() {
        let namespace = Namespace::new("obsidian", "obsidian").unwrap();
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4…
        assert_eq!(
            namespace.content_id(b""),
            "h256:e3b0c44298fc1c149afbf4c8996fb924"
        );
        assert_eq!(
            namespace.content_id(b"jump-cannon"),
            namespace.content_id(b"jump-cannon")
        );
        assert_ne!(namespace.content_id(b"a"), namespace.content_id(b"b"));
        assert_eq!(namespace.content_id(b"anything").len(), "h256:".len() + 32);
    }
}
