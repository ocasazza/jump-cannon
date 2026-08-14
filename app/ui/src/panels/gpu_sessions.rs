//! GPU Sessions panel: per-world GPU compute lifecycle on a multi-user
//! cluster host — Kueue-admitted RayClusters shared across worlds under the
//! standing GPU envelope. Hidden by content, not by presence, in single-user
//! or no-world states (the panel itself stays restore-able from the dock).

use dioxus::prelude::*;
use panel_kit::Spinner;

use crate::{api, client_log, Ctx};

pub fn panel(ctx: Ctx) -> Element {
    let mut status = use_signal(|| None::<serde_json::Value>);
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
            let remote = api::session_manager_url().is_some();
            if remote && world.is_some() && (t, world.clone()) != seen {
                seen = (t, world.clone());
                if let Some(w) = &world {
                    match api::sm_compute_session(w).await {
                        Ok(v) => status.set(Some(v)),
                        Err(e) => note.set(Some(client_log::tagged("gpu-sessions", e))),
                    }
                    loaded.set(true);
                }
            }
            gloo_timers::future::TimeoutFuture::new(3000).await;
        }
    });

    let remote = api::session_manager_url().is_some();
    let multi = ctx.host.read().descriptor().multi_user;
    let world = ctx.active_world.read().clone();
    let st = status.read().clone();
    let message = note.read().clone();

    rsx! {
        div { class: "controls",
            if !remote || !multi {
                div { class: "empty", "GPU sessions need a multi-user session-manager host" }
            } else if world.is_none() {
                div { class: "empty", "open a world in the Worlds panel" }
            } else {
                if let Some(m) = &message {
                    div { class: "note", "{m}" }
                }
                div { class: "server",
                    button {
                        class: "btn",
                        r#type: "button",
                        title: "create the world's RayCluster and wait for Kueue admission",
                        onclick: move |_| {
                            let w = ctx.active_world.read().clone();
                            spawn(async move {
                                if let Some(w) = w {
                                    match api::sm_compute_action(&w, "dispatch").await {
                                        Ok(_) => tick += 1,
                                        Err(e) => note.set(Some(client_log::tagged("gpu-sessions", e))),
                                    }
                                }
                            });
                        },
                        "Dispatch"
                    }
                    button {
                        class: "btn",
                        r#type: "button",
                        title: "delete the RayCluster and release the GPU envelope",
                        onclick: move |_| {
                            let w = ctx.active_world.read().clone();
                            spawn(async move {
                                if let Some(w) = w {
                                    match api::sm_compute_action(&w, "park").await {
                                        Ok(_) => tick += 1,
                                        Err(e) => note.set(Some(client_log::tagged("gpu-sessions", e))),
                                    }
                                }
                            });
                        },
                        "Park"
                    }
                }
                if let Some(v) = &st {
                    pre { class: "session-status",
                        {serde_json::to_string_pretty(v).unwrap_or_default()}
                    }
                } else if *loaded.read() {
                    div { class: "empty", "no session state yet" }
                } else {
                    Spinner { label: "loading session state…" }
                }
                div { class: "note",
                    "worlds share the standing Kueue GPU envelope; idle worlds auto-park"
                }
            }
        }
    }
}
