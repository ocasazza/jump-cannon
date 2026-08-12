//! Branches panel: branch heads of the open world plus the two history
//! operations — merge a branch into `main` and rebase a branch onto `main`.
//! Outcomes (merged commits, conflict counts) surface as notes; conflicts
//! themselves resolve in the Merge panel.

use dioxus::prelude::*;
use graph_vcs::BranchInfo;
use panel_kit::Spinner;

use super::worlds::active_world_id;
use crate::{api, Ctx};

pub fn panel(ctx: Ctx) -> Element {
    let mut branches = use_signal(Vec::<BranchInfo>::new);
    let mut note = use_signal(|| None::<String>);
    let mut tick = use_signal(|| 0u64);
    // Gate the empty state behind the first completed fetch (Spinner while
    // loading, matching the other panels).
    let mut loaded = use_signal(|| false);

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
                        Ok(vcs) => match vcs.branches().await {
                            Ok(mut bs) => {
                                bs.sort_by(|a, b| a.name.0.cmp(&b.name.0));
                                branches.set(bs);
                            }
                            Err(e) => note.set(Some(e.to_string())),
                        },
                        Err(e) => note.set(Some(e.to_string())),
                    }
                } else {
                    branches.set(Vec::new());
                }
                loaded.set(true);
            }
            gloo_timers::future::TimeoutFuture::new(2000).await;
        }
    });

    let list = branches.read().clone();
    let message = note.read().clone();
    let has_world = ctx.active_world.read().is_some();

    rsx! {
        div { class: "controls",
            if !has_world {
                div { class: "empty", "open a world in the Worlds panel" }
            } else {
                if let Some(m) = &message {
                    div { class: "note", "{m}" }
                }
                if list.is_empty() {
                    if *loaded.read() {
                        div { class: "empty", "no branches" }
                    } else {
                        Spinner { label: "loading branches…" }
                    }
                }
                for b in &list {
                    {
                        let name = b.name.0.clone();
                        let head = b.head.0.chars().take(14).collect::<String>();
                        let is_main = name == "main";
                        let merge_name = name.clone();
                        let rebase_name = name.clone();
                        rsx! {
                            div { key: "{name}", class: "kv",
                                span { class: "k", "{name}" }
                                span { class: "v",
                                    "{head}"
                                    if !is_main {
                                        button {
                                            class: "btn",
                                            r#type: "button",
                                            title: "merge this branch into main",
                                            onclick: move |_| {
                                                let from = merge_name.clone();
                                                spawn(async move {
                                                    let Some(wid) = active_world_id(ctx) else { return };
                                                    let host = ctx.host.read().clone();
                                                    let author = api::user_name();
                                                    match host.vcs(&wid).await {
                                                        Ok(vcs) => {
                                                            match vcs
                                                                .merge("main", &from, &author, &format!("merge {from}"))
                                                                .await
                                                            {
                                                                Ok(report) => {
                                                                    let c = report.conflicts.len();
                                                                    note.set(Some(format!(
                                                                        "merge {} → main: {:?} ({} conflict{})",
                                                                        from,
                                                                        report.status,
                                                                        c,
                                                                        if c == 1 { "" } else { "s" },
                                                                    )));
                                                                    tick += 1;
                                                                    crate::spawn_rematerialize_embedded(ctx);
                                                                }
                                                                Err(e) => note.set(Some(e.to_string())),
                                                            }
                                                        }
                                                        Err(e) => note.set(Some(e.to_string())),
                                                    }
                                                });
                                            },
                                            "→ main"
                                        }
                                        button {
                                            class: "btn",
                                            r#type: "button",
                                            title: "rebase this branch onto main",
                                            onclick: move |_| {
                                                let target = rebase_name.clone();
                                                spawn(async move {
                                                    let Some(wid) = active_world_id(ctx) else { return };
                                                    let host = ctx.host.read().clone();
                                                    let author = api::user_name();
                                                    match host.vcs(&wid).await {
                                                        Ok(vcs) => {
                                                            match vcs.rebase(&target, "main", &author).await {
                                                                Ok(report) => {
                                                                    let c = report.conflicts.len();
                                                                    note.set(Some(format!(
                                                                        "rebase {} onto main: {} commit{} replayed, {} conflict{}",
                                                                        target,
                                                                        report.rebased.len(),
                                                                        if report.rebased.len() == 1 { "" } else { "s" },
                                                                        c,
                                                                        if c == 1 { "" } else { "s" },
                                                                    )));
                                                                    tick += 1;
                                                                    crate::spawn_rematerialize_embedded(ctx);
                                                                }
                                                                Err(e) => note.set(Some(e.to_string())),
                                                            }
                                                        }
                                                        Err(e) => note.set(Some(e.to_string())),
                                                    }
                                                });
                                            },
                                            "rebase"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "note", "the served graph tracks main; merges into main rebuild the world snapshot" }
            }
        }
    }
}
