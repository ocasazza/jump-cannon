//! Bottom-right status strip: the echo line (what the last key did, a tip,
//! or the tour prompt), the pending key sequence with its candidates, or an
//! idle hint of the top-level keys. The full reference lives in the Help
//! panel (`key_reference`), toggled by `?`.

use dioxus::prelude::*;

use crate::nav::KEYMAP;
use crate::vi::{EchoKind, HINT};

/// Idle time before the top-level keys are offered, in ms.
const IDLE_MS: u32 = 600;

#[component]
pub(crate) fn WhichKey() -> Element {
    let hint = HINT.read().clone();
    let mut idle = use_signal(|| false);
    // Any change to the typed sequence restarts the idle timer.
    let typed_key = format!("{}|{:?}", hint.typed, hint.count);
    use_effect(use_reactive(&typed_key, move |_| {
        idle.set(false);
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(IDLE_MS).await;
            idle.set(true);
        });
    }));

    let pending = !hint.typed.is_empty() || hint.count.is_some();
    let open = pending || hint.message.is_some() || idle();
    let prefix = format!(
        "{}{}",
        hint.count.map(|c| c.to_string()).unwrap_or_default(),
        hint.typed
    );

    // Rows: (keys, label).
    let rows: Vec<(String, String)> = if pending {
        hint.candidates.iter().map(|c| (c.keys.clone(), c.label.clone())).collect()
    } else if idle() && hint.message.is_none() {
        let mut rows: Vec<_> = KEYMAP
            .rules_in(hint.mode_ix)
            .filter(|r| !r.keys.contains(' '))
            .map(|r| (r.keys.to_owned(), r.label.to_owned()))
            .collect();
        rows.reverse();
        rows.truncate(8);
        rows
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
                if pending || !rows.is_empty() {
                    div { class: "which-key-head",
                        span { class: "which-key-mode", "{hint.mode}" }
                        if pending {
                            span { class: "which-key-typed", "{prefix}" }
                        } else {
                            span { class: "which-key-typed", "? all keys" }
                        }
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
/// mode in authoring order, ungrouped rules last.
pub(crate) fn key_reference() -> Element {
    let mut rows: Vec<(Option<String>, String, String)> = KEYMAP
        .rules_in(0)
        .map(|r| (r.group.map(str::to_owned), r.keys.to_owned(), r.label.to_owned()))
        .collect();
    rows.reverse();
    rows.sort_by_key(|(g, ..)| g.is_none());
    let mut last_group: Option<Option<String>> = None;
    rsx! {
        dl { class: "key-reference", "aria-label": "Keyboard reference",
            for (group , keys , label) in rows {
                if last_group.as_ref() != Some(&group) {
                    {last_group = Some(group.clone()); rsx! { dt { key: "g-{keys}", "{group.clone().unwrap_or_else(|| \"Other\".into())}" } }}
                }
                dd { key: "{keys}",
                    kbd { "{keys}" }
                    span { "{label}" }
                }
            }
        }
    }
}
