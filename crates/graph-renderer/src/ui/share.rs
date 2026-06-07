//! Shareable state hash / link codec.
//!
//! Encodes an [`AppState`] into a short, URL-fragment-safe string and decodes it
//! back. The pipeline is:
//!
//! ```text
//! AppState ──serde_json (compact)──► bytes
//!          ──DEFLATE (miniz_oxide)──► compressed bytes
//!          ──base64url (no padding)──► hash string
//! ```
//!
//! and the exact inverse for decode. Compact JSON (not YAML) is used as the
//! payload because it is markedly smaller before compression, and the codec is
//! about producing a *short* shareable token.
//!
//! On WASM the hash rides as the `#s=<hash>` URL fragment (read at startup,
//! produced by the "Copy share link" button). Everything here is platform
//! agnostic — the web_sys URL/clipboard plumbing lives in the Instances panel
//! and `web.rs`.

use base64::Engine as _;

use crate::ui::state::AppState;

/// URL-fragment key the encoded state rides under: `#s=<hash>`.
pub const FRAGMENT_KEY: &str = "s";

/// base64url alphabet, no padding — fragment-safe (`-`/`_`, no `+`/`/`/`=`).
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// DEFLATE compression level (0..=10 in miniz_oxide). 9 is the strongest
/// non-experimental ratio; the payloads are small so the cost is negligible.
const DEFLATE_LEVEL: u8 = 9;

/// Encode an [`AppState`] into a short, URL-fragment-safe hash string.
///
/// Round-trips with [`decode`] for the persisted (non-`#[serde(skip)]`) subset
/// of `AppState`: skipped session-scratch fields reset to their defaults on the
/// far side, which is the same contract as the YAML export/import and
/// sessionStorage paths.
pub fn encode(state: &AppState) -> Result<String, String> {
    // Use the sanitized JSON encoder so egui's non-finite cached Rects (which
    // `serde_json` writes as `null`, then refuses to deserialize) round-trip.
    let json = crate::ui::state::to_json_sanitized(state).map_err(|e| format!("serialize: {e}"))?;
    let compressed = miniz_oxide::deflate::compress_to_vec(json.as_bytes(), DEFLATE_LEVEL);
    Ok(B64.encode(compressed))
}

/// Decode a hash string produced by [`encode`] back into an [`AppState`].
///
/// Tolerates a leading `#`, a leading `s=` (or `#s=`) fragment prefix, and
/// surrounding whitespace, so a user can paste either the bare hash or the whole
/// fragment.
pub fn decode(hash: &str) -> Result<AppState, String> {
    let cleaned = strip_fragment(hash);
    if cleaned.is_empty() {
        return Err("empty hash".to_string());
    }
    let compressed = B64
        .decode(cleaned.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    let json = miniz_oxide::inflate::decompress_to_vec(&compressed)
        .map_err(|e| format!("inflate: {e:?}"))?;
    // Sanitize any non-finite-derived nulls (belt-and-suspenders: `encode`
    // already sanitizes, but a hand-rolled / older blob may not have).
    let mut value: serde_json::Value =
        serde_json::from_slice(&json).map_err(|e| format!("parse: {e}"))?;
    crate::ui::state::sanitize_nonfinite(&mut value);
    serde_json::from_value(value).map_err(|e| format!("deserialize: {e}"))
}

/// Strip a leading `#`, an `s=` / `#s=` fragment prefix, and surrounding
/// whitespace from a user-pasted hash/link so both forms decode.
fn strip_fragment(input: &str) -> &str {
    let s = input.trim();
    // Whole URL pasted? Take everything after the last `#`.
    let s = match s.rsplit_once('#') {
        Some((_, frag)) => frag,
        None => s,
    };
    // `s=<hash>` fragment form.
    let prefix = format!("{FRAGMENT_KEY}=");
    s.strip_prefix(&prefix).unwrap_or(s).trim()
}

/// Build a full shareable link from a hash. On WASM the `origin` is the page
/// origin (from `web_sys`); on native the caller passes whatever it likes (the
/// Instances panel just shows the bare hash there).
pub fn link_for(origin: &str, hash: &str) -> String {
    format!("{origin}/#{FRAGMENT_KEY}={hash}")
}

/// WASM-only: read the current page's `#s=<hash>` URL fragment, if present and
/// non-empty. Returns `None` on native or when no such fragment exists, so a
/// normal load is unaffected.
#[cfg(target_arch = "wasm32")]
pub fn fragment_from_location() -> Option<String> {
    let window = web_sys::window()?;
    let hash = window.location().hash().ok()?; // e.g. "#s=AbCd"
    let cleaned = strip_fragment(&hash);
    // Only treat it as a state fragment when the `s=` key was actually present;
    // `strip_fragment` returns the raw fragment otherwise (which we ignore).
    let raw = hash.trim_start_matches('#');
    if raw.starts_with(&format!("{FRAGMENT_KEY}=")) && !cleaned.is_empty() {
        Some(cleaned.to_string())
    } else {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn fragment_from_location() -> Option<String> {
    None
}

/// Parse `?soup=<n>[&morphology=<m>]` from the page URL — the boot trigger that
/// makes the dev server come up already assembling a particle soup (n particles,
/// morphology chains|sheet|tube|vesicle, default "sheet"). Returns `None` when
/// absent, on native, or when `n` doesn't parse, so a normal load is unaffected.
#[cfg(target_arch = "wasm32")]
pub fn soup_request_from_location() -> Option<(u32, String)> {
    let window = web_sys::window()?;
    let search = window.location().search().ok()?; // e.g. "?soup=50000&morphology=sheet"
    let q = search.trim_start_matches('?');
    let mut n = None;
    let mut morphology = String::from("sheet");
    for kv in q.split('&') {
        let mut it = kv.splitn(2, '=');
        match (it.next(), it.next()) {
            (Some("soup"), Some(v)) => n = v.trim().parse::<u32>().ok(),
            (Some("morphology"), Some(v)) if !v.is_empty() => morphology = v.to_string(),
            _ => {}
        }
    }
    n.map(|n| (n, morphology))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn soup_request_from_location() -> Option<(u32, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-default state exercising several persisted parameter families
    /// (style, layout settings, generate/seed sources, panel placement).
    fn sample_state() -> AppState {
        let mut s = AppState::default();
        s.style.size_mul = 1.7;
        s.style.edge_width = 3.3;
        s.style.color_by = crate::ui::state::ColorBy::Folder;
        s.set_section_open(crate::ui::state::Section::Layout, true);
        s.filter_strip_open = false;
        s.status_footer_open = true;
        s.tag_browser_query = "cooking".to_string();
        s.layout.active = "geometric".to_string();
        s.layout.settings.insert(
            "geometric".to_string(),
            serde_json::json!({ "use_gpu": true }),
        );
        s.generate.editor.source = "# my generator\n{ nodes = []; links = []; }".to_string();
        s.seed.strategy = crate::ui::state::SeedStrategy::BuiltIn(2);
        s.seed.editor.source = "# my seed\n[]".to_string();
        s
    }

    /// Canonical (sanitized) JSON form — the persisted representation that both
    /// the round-trip and the share codec are defined against. Identity is
    /// checked on THIS form (non-finite Rect nulls already mapped to `0.0`).
    fn canon(s: &AppState) -> String {
        crate::ui::state::to_json_sanitized(s).unwrap()
    }

    /// JSON round-trip baseline for the chosen non-default state: the persisted
    /// (sanitized) subset survives serialize → deserialize (the same contract
    /// the hash codec relies on). `#[serde(skip)]` fields reset to defaults on
    /// both sides.
    #[test]
    fn json_roundtrip_is_identity_for_persisted_subset() {
        let original = sample_state();
        let json = canon(&original);
        let back: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(json, canon(&back));
    }

    /// encode → decode → AppState equals the original on the persisted subset.
    #[test]
    fn hash_roundtrip_is_identity() {
        let original = sample_state();
        let hash = encode(&original).expect("encode");
        let back = decode(&hash).expect("decode");
        assert_eq!(
            canon(&original),
            canon(&back),
            "hash round-trip must preserve the persisted subset"
        );
    }

    /// The hash is base64url-only (fragment-safe) and meaningfully smaller than
    /// the raw JSON it encodes.
    #[test]
    fn hash_is_fragment_safe_and_compact() {
        let original = sample_state();
        let json_len = serde_json::to_string(&original).unwrap().len();
        let hash = encode(&original).unwrap();
        // base64url alphabet only: no `+`, `/`, `=`, or whitespace.
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "hash must be base64url: {hash}"
        );
        // DEFLATE on a JSON blob this size should beat the raw text. (Sanity,
        // not a hard guarantee — but our default-heavy states compress well.)
        assert!(
            hash.len() < json_len,
            "hash ({}) should be smaller than raw JSON ({json_len})",
            hash.len()
        );
    }

    /// Decode tolerates the `#s=` / `s=` fragment forms and a whole pasted link.
    #[test]
    fn decode_tolerates_fragment_prefixes() {
        let original = sample_state();
        let hash = encode(&original).unwrap();
        let want = canon(&original);
        for variant in [
            hash.clone(),
            format!("#{}", hash),
            format!("s={}", hash),
            format!("#s={}", hash),
            link_for("https://example.com", &hash),
            format!("  #s={}  ", hash),
        ] {
            let back = decode(&variant).unwrap_or_else(|e| panic!("decode {variant:?}: {e}"));
            assert_eq!(want, canon(&back), "variant {variant:?}");
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode("").is_err());
        assert!(decode("not!!base64!!").is_err());
        // Valid base64 but not DEFLATE.
        assert!(decode("aGVsbG8").is_err());
    }

    #[test]
    fn link_format() {
        assert_eq!(link_for("https://x.io", "ABC"), "https://x.io/#s=ABC");
    }
}
