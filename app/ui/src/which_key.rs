//! Bottom-right status strip: the echo line (what the last key did, a tip,
//! or the tour prompt) and the candidates of a pending key sequence. It never
//! lists the keymap on its own; the full reference lives in the Help panel
//! (`key_reference`), toggled by `?`.

use dioxus::prelude::*;

use crate::nav::KEYMAP;
use crate::vi::{EchoKind, HINT};

#[component]
pub(crate) fn WhichKey() -> Element {
    let hint = HINT.read().clone();

    let pending = !hint.typed.is_empty() || hint.count.is_some();
    let open = pending || hint.message.is_some();
    let prefix = format!(
        "{}{}",
        hint.count.map(|c| c.to_string()).unwrap_or_default(),
        hint.typed
    );

    // Rows: (keys, label).
    let rows: Vec<(String, String)> = if pending {
        hint.candidates.iter().map(|c| (c.keys.clone(), c.label.clone())).collect()
    } else {
        Vec::new()
    };

    rsx! {
        // The live region stays mounted so its first content is announced.
        div {
            class: "which-key",
            role: "status",
            "aria-live": "polite",
            "aria-atomic": "true",
            "aria-label": "Keyboard help",
            "data-testid": "which-key",
            "data-open": if open { "true" } else { "false" },
            if open {
                if let Some(msg) = &hint.message {
                    div {
                        class: "which-key-echo",
                        "data-kind": match msg.kind {
                            EchoKind::Did => "did",
                            EchoKind::Refused => "refused",
                            EchoKind::Tip => "tip",
                            EchoKind::Tour => "tour",
                        },
                        "{msg.text}"
                    }
                }
                if pending {
                    div { class: "which-key-head",
                        span { class: "which-key-mode", "{hint.mode}" }
                        span { class: "which-key-typed", "{prefix}" }
                    }
                    ul { class: "which-key-list",
                        for (keys , label) in rows {
                            li { key: "{keys}",
                                kbd { "{keys}" }
                                span { class: "which-key-intent", "{label}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The grouped key reference for the Help panel: every rule of the initial
/// mode, grouped and ordered by panel-kit so it cannot drift from the keymap.
pub(crate) fn key_reference() -> Element {
    let groups = KEYMAP.key_reference(0);
    rsx! {
        dl { class: "key-reference", "aria-label": "Keyboard reference",
            for group in groups {
                dt { key: "g-{group.group.unwrap_or(\"\")}", "{group.group.unwrap_or(\"Other\")}" }
                for (keys , label) in group.rows {
                    dd { key: "{keys}",
                        kbd { "{keys}" }
                        span { "{label}" }
                    }
                }
            }
        }
    }
}
