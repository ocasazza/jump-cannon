//! History panel: commit log of the open world's branch, with branch
//! selection and branch-from-commit creation. Log browsing is read-only —
//! the served graph always tracks `main` (the WorldImporter's branch).

use dioxus::prelude::*;
use graph_vcs::Commit;

use super::worlds::active_world_id;
use crate::Ctx;

pub fn panel(ctx: Ctx) -> Element {
    let mut branch = use_signal(|| "main".to_string());
    let mut branches = use_signal(Vec::<String>::new);
    let mut commits = use_signal(Vec::<Commit>::new);
    let mut error = use_signal(|| None::<String>);
    let mut new_branch = use_signal(String::new);
    let mut tick = use_signal(|| 0u64);

    use_future(move || async move {
        let mut seen = (u64::MAX, String::new(), Option::<String>::None);
        loop {
            let world = ctx.active_world.read().clone();
            let b = branch.read().clone();
            let t = *tick.read();
            if (t, b.clone(), world.clone()) != seen {
                seen = (t, b.clone(), world.clone());
                let Some(wid) = active_world_id(ctx) else {
                    commits.set(Vec::new());
                    branches.set(Vec::new());
                    gloo_timers::future::TimeoutFuture::new(1500).await;
                    continue;
                };
                let host = ctx.host.read().clone();
                match host.vcs(&wid).await {
                    Ok(vcs) => {
                        match vcs.branches().await {
                            Ok(bs) => {
                                let mut names: Vec<String> =
                                    bs.iter().map(|b| b.name.0.clone()).collect();
                                names.sort();
                                branches.set(names);
                            }
                            Err(e) => error.set(Some(e.to_string())),
                        }
                        match vcs.log(&b, 200).await {
                            Ok(cs) => {
                                commits.set(cs);
                                error.set(None);
                            }
                            Err(e) => error.set(Some(e.to_string())),
                        }
                    }
                    Err(e) => error.set(Some(e.to_string())),
                }
            }
            gloo_timers::future::TimeoutFuture::new(1500).await;
        }
    });

    let err = error.read().clone();
    let log = commits.read().clone();
    let branch_names = branches.read().clone();
    let current_branch = branch.read().clone();
    let has_world = ctx.active_world.read().is_some();

    rsx! {
        div { class: "controls",
            if !has_world {
                div { class: "empty", "open a world in the Worlds panel" }
            } else {
                div { class: "server",
                    for name in branch_names {
                        {
                            let active = name == current_branch;
                            let pick = name.clone();
                            rsx! {
                                button {
                                    key: "{name}",
                                    class: if active { "btn branch active" } else { "btn branch" },
                                    r#type: "button",
                                    onclick: move |_| branch.set(pick.clone()),
                                    "{name}"
                                }
                            }
                        }
                    }
                }
                if let Some(e) = &err {
                    div { class: "note", "error: {e}" }
                }
                if log.is_empty() && err.is_none() {
                    div { class: "empty", "no commits on {current_branch}" }
                }
                for c in log {
                    {
                        let cid = c.id.0.clone();
                        let short = cid.chars().take(14).collect::<String>();
                        let change = c.change_id.0.clone();
                        let conflicted = !c.conflicts.is_empty();
                        let label = format!(
                            "{} {} {} · {} ops{}",
                            c.author,
                            if c.message.is_empty() { "(no message)" } else { &c.message },
                            c.timestamp_ms,
                            c.ops.len(),
                            if conflicted { " · CONFLICT" } else { "" },
                        );
                        rsx! {
                            div { key: "{cid}", class: "kv",
                                span { class: "k", title: "change {change}", "{short}" }
                                span { class: "v",
                                    "{label}"
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        disabled: new_branch.read().trim().is_empty(),
                                        onclick: move |_| {
                                            let name = new_branch.read().trim().to_string();
                                            let from = c.id.clone();
                                            spawn(async move {
                                                let Some(wid) = active_world_id(ctx) else { return };
                                                let host = ctx.host.read().clone();
                                                match host.vcs(&wid).await {
                                                    Ok(vcs) => {
                                                        match vcs.create_branch(&name, &from).await {
                                                            Ok(()) => {
                                                                new_branch.set(String::new());
                                                                tick += 1;
                                                                crate::spawn_rematerialize_embedded(ctx);
                                                            }
                                                            Err(e) => error.set(Some(e.to_string())),
                                                        }
                                                    }
                                                    Err(e) => error.set(Some(e.to_string())),
                                                }
                                            });
                                        },
                                        "Branch here"
                                    }
                                }
                            }
                        }
                    }
                }
                input {
                    aria_label: "New branch name",
                    placeholder: "new branch name (then pick a commit)",
                    value: "{new_branch}",
                    oninput: move |event| new_branch.set(event.value()),
                }
            }
        }
    }
}
