//! Merge panel: recorded conflicts on the open world's current branch and
//! their resolution. Conflicts are first-class (stored, non-blocking):
//! resolving commits the chosen side and clears them.

use dioxus::prelude::*;
use graph_vcs::{Conflict, ConflictResolution, NodeId, ResolvedNode};

use super::worlds::active_world_id;
use crate::{api, Ctx};

pub fn panel(ctx: Ctx) -> Element {
    let mut conflicts = use_signal(Vec::<Conflict>::new);
    let mut note = use_signal(|| None::<String>);
    let tick = use_signal(|| 0u64);

    use_future(move || async move {
        let mut seen = (u64::MAX, Option::<String>::None);
        loop {
            let world = ctx.active_world.read().clone();
            let t = *tick.read();
            if (t, world.clone()) != seen {
                seen = (t, world.clone());
                if let Some(wid) = active_world_id(ctx) {
                    let host = ctx.host.read().clone();
                    match host.vcs(&wid).await {
                        Ok(vcs) => match vcs.conflicts("main").await {
                            Ok(cs) => conflicts.set(cs),
                            Err(e) => note.set(Some(e.to_string())),
                        },
                        Err(e) => note.set(Some(e.to_string())),
                    }
                } else {
                    conflicts.set(Vec::new());
                }
            }
            gloo_timers::future::TimeoutFuture::new(2000).await;
        }
    });

    let list = conflicts.read().clone();
    let message = note.read().clone();
    let has_world = ctx.active_world.read().is_some();

    let resolve = move |node: NodeId, choice: ResolvedNode| {
        // Rebind the Copy signals so the closure stays `Fn` (moving them into
        // the spawned future would make it `FnOnce`).
        let mut tick = tick;
        let mut note = note;
        spawn(async move {
            let Some(wid) = active_world_id(ctx) else { return };
            let host = ctx.host.read().clone();
            match host.vcs(&wid).await {
                Ok(vcs) => {
                    match vcs
                        .resolve("main", vec![ConflictResolution { node_id: node, choice }], &api::user_name())
                        .await
                    {
                        Ok(_) => {
                            note.set(Some("resolution committed".to_string()));
                            tick += 1;
                            crate::spawn_rematerialize_embedded(ctx);
                        }
                        Err(e) => note.set(Some(e.to_string())),
                    }
                }
                Err(e) => note.set(Some(e.to_string())),
            }
        });
    };

    rsx! {
        div { class: "controls",
            if !has_world {
                div { class: "empty", "open a world in the Worlds panel" }
            } else {
                if let Some(m) = &message {
                    div { class: "note", "{m}" }
                }
                if list.is_empty() {
                    div { class: "empty", "no recorded conflicts on main" }
                }
                for c in &list {
                    {
                        let node = c.node_id.clone();
                        let ours = c
                            .ours
                            .as_ref()
                            .map(|n| n.meta.title.clone())
                            .unwrap_or_else(|| "—".to_string());
                        let theirs = c
                            .theirs
                            .as_ref()
                            .map(|n| n.meta.title.clone())
                            .unwrap_or_else(|| "—".to_string());
                        rsx! {
                            div { key: "{node.0}", class: "conflict-row",
                                div { class: "kv",
                                    span { class: "k", "{node.0}" }
                                    span { class: "v", "ours: {ours} · theirs: {theirs}" }
                                }
                                div { class: "server",
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        disabled: c.ours.is_none(),
                                        onclick: {
                                            let n = node.clone();
                                            move |_| resolve(n.clone(), ResolvedNode::Ours)
                                        },
                                        "keep ours"
                                    }
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        disabled: c.theirs.is_none(),
                                        onclick: {
                                            let n = node.clone();
                                            move |_| resolve(n.clone(), ResolvedNode::Theirs)
                                        },
                                        "keep theirs"
                                    }
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        onclick: {
                                            let n = node.clone();
                                            move |_| resolve(n.clone(), ResolvedNode::Deleted)
                                        },
                                        "delete"
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "note", "resolutions land as a commit on main and rebuild the served snapshot" }
            }
        }
    }
}
