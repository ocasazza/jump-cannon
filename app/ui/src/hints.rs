//! Header hint bar: a global registry any component can publish a
//! contextual hint into (what is under the pointer / focused, and which
//! hotkeys act on it). The topbar renders the most recently published
//! entry; each publisher owns one slot keyed by its `source`.

use dioxus::prelude::*;

use crate::proto::NodeMeta;

/// One contextual hint. `hotkeys` are `(action label, key chord)` pairs
/// rendered as `<kbd>` chips.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Hint {
    pub(crate) source: &'static str,
    pub(crate) title: String,
    pub(crate) body: Option<String>,
    pub(crate) hotkeys: Vec<(&'static str, &'static str)>,
}

/// Publish order: the last entry is the one shown.
static HINTS: GlobalSignal<Vec<Hint>> = Signal::global(Vec::new);

/// Publish `hint`, replacing any earlier entry from the same source and
/// making it the shown one. No-op write when the shown entry is unchanged.
pub(crate) fn publish(hint: Hint) {
    {
        let cur = HINTS.peek();
        if cur.last() == Some(&hint) {
            return;
        }
    }
    let mut w = HINTS.write();
    w.retain(|h| h.source != hint.source);
    w.push(hint);
}

/// Drop `source`'s entry; the previously published hint (if any) shows.
pub(crate) fn clear(source: &'static str) {
    if HINTS.peek().iter().any(|h| h.source == source) {
        HINTS.write().retain(|h| h.source != source);
    }
}

/// Hotkey chip shared by every node hint.
pub(crate) const VIEW_NODE: (&str, &str) = ("View Node", "Super+V");

/// Body excerpt budget (chars) for the single-line node summary.
const EXCERPT_CHARS: usize = 96;
/// Tags shown before the summary falls back to a "+N" count.
const MAX_TAGS: usize = 3;

/// Node hint from `/node/:id` meta when available, else the bare id
/// (browser-only graphs have no meta endpoint).
pub(crate) fn node_hint(source: &'static str, id: &str, meta: Option<&NodeMeta>) -> Hint {
    let Some(m) = meta else {
        return Hint {
            source,
            title: id.to_string(),
            body: None,
            hotkeys: vec![VIEW_NODE],
        };
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = m.doctype.as_deref().filter(|d| !d.is_empty()) {
        parts.push(d.to_string());
    }
    if !m.folder.is_empty() {
        parts.push(m.folder.clone());
    }
    if !m.tags.is_empty() {
        let mut tags = m
            .tags
            .iter()
            .take(MAX_TAGS)
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(" ");
        if m.tags.len() > MAX_TAGS {
            tags.push_str(&format!(" +{}", m.tags.len() - MAX_TAGS));
        }
        parts.push(tags);
    }
    if let Some(line) = first_line(&m.body) {
        parts.push(line);
    }
    Hint {
        source,
        title: node_title(m),
        body: (!parts.is_empty()).then(|| parts.join(" · ")),
        hotkeys: vec![VIEW_NODE],
    }
}

/// Title fallback chain: title → path file stem → id.
fn node_title(meta: &NodeMeta) -> String {
    if !meta.title.is_empty() {
        return meta.title.clone();
    }
    if !meta.path.is_empty() {
        if let Some(stem) = std::path::Path::new(&meta.path).file_stem().and_then(|s| s.to_str()) {
            return stem.to_string();
        }
    }
    meta.id.clone()
}

/// First non-blank body line, cut to `EXCERPT_CHARS` with an ellipsis.
fn first_line(body: &str) -> Option<String> {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut out: String = line.chars().take(EXCERPT_CHARS).collect();
    if line.chars().count() > EXCERPT_CHARS {
        out.push('…');
    }
    Some(out)
}

/// Topbar mount: the shown hint as title · body plus hotkey chips. Renders
/// the (empty) region unconditionally so the header never reflows.
pub(crate) fn header_bar() -> Element {
    let hints = HINTS.read();
    let shown = hints.last();
    rsx! {
        div { class: "hintbar", role: "status", aria_live: "polite",
            if let Some(h) = shown {
                span { class: "hintbar-title", "{h.title}" }
                if let Some(b) = &h.body {
                    span { class: "hintbar-body", "{b}" }
                }
                for (label, keys) in h.hotkeys.iter() {
                    span { class: "hintbar-key",
                        "{label}"
                        kbd { "{keys}" }
                    }
                }
            }
        }
    }
}
