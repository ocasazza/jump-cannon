//! Worlds panel: list/create/open/close versioned worlds on the active
//! `WorldHost`. Membership state renders only for multi-user hosts — the
//! embedded single-user host never mounts session-management UI.
//!
//! Embedded-host extras (no session-manager URL configured): per-world
//! JSON export / file import (through `EmbeddedSessionManager`'s inherent
//! methods — the `WorldHost` trait stays stable), and a minimal commit
//! editor that lands `GraphOp` batches on the open world's `main`.

use dioxus::prelude::*;
use graph_vcs::{GraphOp, NodeId, VaultEdge, VaultNode};
use panel_kit::Spinner;
use session_manager::{UserIdentity, WorldId, WorldSpec};

use super::instances::download_text;
use crate::{api, clear_world_base, open_world_in_view, spawn_rematerialize_embedded, Ctx};

#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    name: String,
    description: Option<String>,
    branches: usize,
}

/// Shared fetch: current world listing of the active host.
async fn fetch_rows(ctx: Ctx) -> Result<Vec<Row>, String> {
    let host = ctx.host.read().clone();
    let worlds = host.worlds().await.map_err(|e| e.to_string())?;
    Ok(worlds
        .into_iter()
        .map(|w| Row {
            id: w.id.0,
            name: w.name,
            description: w.description,
            branches: w.branches,
        })
        .collect())
}

/// Shared helper: the open world's validated id, when one is open.
pub(crate) fn active_world_id(ctx: Ctx) -> Option<WorldId> {
    ctx.active_world
        .read()
        .as_deref()
        .and_then(|w| WorldId::parse(w).ok())
}

pub fn panel(mut ctx: Ctx) -> Element {
    let mut rows = use_signal(Vec::<Row>::new);
    let mut error = use_signal(|| None::<String>);
    let mut name = use_signal(String::new);
    let mut desc = use_signal(String::new);
    let mut tick = use_signal(|| 0u64);
    // Gate the empty state behind the first completed fetch so it doesn't
    // flash before the list resolves (app convention: Spinner while loading).
    let mut loaded = use_signal(|| false);
    // Commit-editor form state lives here, not in `commit_editor`: the
    // editor section renders conditionally (embedded host + open world), and
    // hooks must stay unconditional within one scope.
    let editor = EditorState {
        node_id: use_signal(String::new),
        node_title: use_signal(String::new),
        node_tags: use_signal(String::new),
        edge_source: use_signal(String::new),
        edge_target: use_signal(String::new),
        note: use_signal(|| None::<String>),
    };

    // Poll the world list; `tick` forces an immediate refresh after a
    // mutation instead of waiting out the interval.
    use_future(move || async move {
        let mut seen_tick = u64::MAX;
        loop {
            let t = *tick.read();
            if t != seen_tick {
                seen_tick = t;
                match fetch_rows(ctx).await {
                    Ok(rs) => {
                        rows.set(rs);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e)),
                }
                loaded.set(true);
            }
            gloo_timers::future::TimeoutFuture::new(1500).await;
        }
    });

    let host_descriptor = ctx.host.read().descriptor();
    let embedded = ctx.embedded.read().clone();
    let active = ctx.active_world.read().clone();
    let list = rows.read().clone();
    let err = error.read().clone();

    rsx! {
        div { class: "controls",
            div { class: "server",
                input {
                    aria_label: "World name",
                    placeholder: "world name",
                    value: "{name}",
                    oninput: move |event| name.set(event.value()),
                }
                button {
                    class: "btn",
                    r#type: "button",
                    disabled: name.read().trim().is_empty(),
                    onclick: move |_| {
                        let spec = WorldSpec {
                            name: name.read().trim().to_string(),
                            description: {
                                let d = desc.read().trim().to_string();
                                if d.is_empty() { None } else { Some(d) }
                            },
                        };
                        spawn(async move {
                            let host = ctx.host.read().clone();
                            match host.open_world(spec).await {
                                Ok(_) => {
                                    name.set(String::new());
                                    desc.set(String::new());
                                    tick += 1;
                                }
                                Err(e) => error.set(Some(e.to_string())),
                            }
                        });
                    },
                    "Create"
                }
            }
            input {
                aria_label: "World description",
                placeholder: "description (optional)",
                value: "{desc}",
                oninput: move |event| desc.set(event.value()),
            }
            if let Some(e) = &err {
                div { class: "note", "error: {e}" }
            }
            if list.is_empty() && err.is_none() {
                if *loaded.read() {
                    div { class: "empty", "no worlds yet — create one above" }
                } else {
                    Spinner { label: "loading worlds…" }
                }
            }
            for row in list {
                {
                    let is_active = active.as_deref() == Some(row.id.as_str());
                    let id = row.id.clone();
                    let id_for_close = row.id.clone();
                    let id_for_export = row.id.clone();
                    let em_for_export = embedded.clone();
                    rsx! {
                        div { key: "{row.id}", class: "kv",
                            span { class: "k",
                                if is_active { "● {row.name}" } else { "{row.name}" }
                            }
                            span { class: "v",
                                "{row.branches} branch(es)"
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    disabled: is_active,
                                    onclick: move |_| {
                                        let world = id.clone();
                                        // Attach a session up front on
                                        // multi-user hosts; single-user hosts
                                        // keep their one implicit session.
                                        let joined = world.clone();
                                        spawn(async move {
                                            let host = ctx.host.read().clone();
                                            if let Some(dir) = host.sessions() {
                                                if let Ok(wid) = WorldId::parse(&joined) {
                                                    let user = UserIdentity {
                                                        name: api::user_name(),
                                                        groups: Vec::new(),
                                                    };
                                                    let _ = dir.join(&wid, &user).await;
                                                }
                                            }
                                        });
                                        open_world_in_view(ctx, world);
                                    },
                                    "Open"
                                }
                                button {
                                    class: "btn",
                                    r#type: "button",
                                    onclick: move |_| {
                                        let world = id_for_close.clone();
                                        let was_active = is_active;
                                        spawn(async move {
                                            let host = ctx.host.read().clone();
                                            if let Ok(wid) = WorldId::parse(&world) {
                                                match host.close_world(&wid).await {
                                                    Ok(()) => {
                                                        if was_active {
                                                            clear_world_base(ctx);
                                                            ctx.graph.set(None);
                                                        }
                                                        tick += 1;
                                                    }
                                                    Err(e) => error.set(Some(e.to_string())),
                                                }
                                            }
                                        });
                                    },
                                    "Close"
                                }
                                if let Some(em) = em_for_export.clone() {
                                    button {
                                        class: "btn",
                                        r#type: "button",
                                        title: "Download the world's full VCS history as JSON",
                                        onclick: move |_| {
                                            let em = em.clone();
                                            let world = id_for_export.clone();
                                            spawn(async move {
                                                let Ok(wid) = WorldId::parse(&world) else { return };
                                                let result = em
                                                    .export_world(&wid)
                                                    .map_err(|e| e.to_string())
                                                    .and_then(|export| {
                                                        serde_json::to_string_pretty(&export)
                                                            .map_err(|e| format!("encode: {e}"))
                                                    });
                                                match result {
                                                    Ok(json) => {
                                                        if let Err(e) = download_text(
                                                            &format!("{}.world.json", wid.0),
                                                            "application/json",
                                                            &json,
                                                        ) {
                                                            error.set(Some(format!("download: {e}")));
                                                        }
                                                    }
                                                    Err(e) => error.set(Some(e)),
                                                }
                                            });
                                        },
                                        "Export"
                                    }
                                }
                            }
                        }
                        if let Some(d) = &row.description {
                            div { class: "note", "{d}" }
                        }
                    }
                }
            }
            if let Some(em) = embedded.clone() {
                div { class: "server",
                    label { class: "btn",
                        title: "Pick an exported .world.json file and import it as a new world",
                        "⬆ Import world"
                        input {
                            r#type: "file",
                            accept: ".json,application/json",
                            style: "display:none",
                            onchange: move |evt| {
                                let em = em.clone();
                                if let Some(engine) = evt.files() {
                                    spawn(async move {
                                        for file_name in engine.files() {
                                            match engine.read_file_to_string(&file_name).await {
                                                Some(text) => {
                                                    let result = serde_json::from_str::<
                                                        session_manager::WorldExport,
                                                    >(&text)
                                                    .map_err(|e| format!("parse: {e}"))
                                                    .and_then(|export| {
                                                        let name = export.name.clone();
                                                        em.import_world(&name, export)
                                                            .map_err(|e| e.to_string())
                                                    });
                                                    match result {
                                                        Ok(_) => tick += 1,
                                                        Err(e) => error.set(Some(e)),
                                                    }
                                                }
                                                None => {
                                                    error.set(Some("upload: read failed".to_string()));
                                                }
                                            }
                                        }
                                    });
                                }
                            },
                        }
                    }
                }
            }
            if host_descriptor.multi_user {
                div { class: "note",
                    "multi-user host ({host_descriptor.id:?}): you are \"{api::user_name()}\"; \
                     world creators hold write access"
                }
            } else {
                div { class: "note",
                    "single-user embedded host — worlds persist in this browser \
                     (localStorage snapshot per commit)"
                }
            }
            if embedded.is_some() && active.is_some() {
                { commit_editor(ctx, editor) }
            }
        }
    }
}

/// Form state for the commit editor (created unconditionally in `panel`).
#[derive(Clone, Copy)]
struct EditorState {
    node_id: Signal<String>,
    node_title: Signal<String>,
    node_tags: Signal<String>,
    edge_source: Signal<String>,
    edge_target: Signal<String>,
    note: Signal<Option<String>>,
}

/// Minimal commit editor for the open embedded world: add-node and add-edge
/// forms landing `GraphOp` batches on `main`, plus a delete action for the
/// canvas selection. Every successful commit re-materializes the canvas
/// (and, on the persistent backend, snapshots the world to localStorage).
fn commit_editor(ctx: Ctx, editor: EditorState) -> Element {
    let EditorState {
        mut node_id,
        mut node_title,
        mut node_tags,
        mut edge_source,
        mut edge_target,
        note,
    } = editor;
    let selected = ctx.selected.read().clone();

    // Signals are `Copy`, so this closure is too and can serve every button.
    let commit_ops = move |ops: Vec<GraphOp>, message: String| {
        let mut note = note;
        spawn(async move {
            let Some(wid) = active_world_id(ctx) else { return };
            let host = ctx.host.read().clone();
            let author = api::user_name();
            match host.vcs(&wid).await {
                Ok(vcs) => match vcs.commit("main", ops, &author, &message).await {
                    Ok(_) => {
                        note.set(Some(format!("committed: {message}")));
                        spawn_rematerialize_embedded(ctx);
                    }
                    Err(e) => note.set(Some(e.to_string())),
                },
                Err(e) => note.set(Some(e.to_string())),
            }
        });
    };

    let message = note.read().clone();
    rsx! {
        div { class: "inst-subhead", "Commit to main" }
        if let Some(m) = &message {
            div { class: "note", "{m}" }
        }
        div { class: "server",
            input {
                aria_label: "Node id",
                placeholder: "node id",
                value: "{node_id}",
                oninput: move |event| node_id.set(event.value()),
            }
            input {
                aria_label: "Node title",
                placeholder: "title",
                value: "{node_title}",
                oninput: move |event| node_title.set(event.value()),
            }
        }
        div { class: "server",
            input {
                aria_label: "Node tags",
                placeholder: "tags (comma separated)",
                value: "{node_tags}",
                oninput: move |event| node_tags.set(event.value()),
            }
            button {
                class: "btn",
                r#type: "button",
                disabled: node_id.read().trim().is_empty(),
                onclick: move |_| {
                    let id = node_id.read().trim().to_string();
                    let title = node_title.read().trim().to_string();
                    let mut node = VaultNode::default();
                    node.id = id.clone();
                    node.meta.title = if title.is_empty() { id.clone() } else { title };
                    node.meta.tags = node_tags
                        .read()
                        .split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                    commit_ops(vec![GraphOp::UpsertNode(node)], format!("add node {id}"));
                    node_id.set(String::new());
                    node_title.set(String::new());
                    node_tags.set(String::new());
                },
                "Add node"
            }
        }
        div { class: "server",
            input {
                aria_label: "Edge source",
                placeholder: "edge source",
                value: "{edge_source}",
                oninput: move |event| edge_source.set(event.value()),
            }
            input {
                aria_label: "Edge target",
                placeholder: "edge target",
                value: "{edge_target}",
                oninput: move |event| edge_target.set(event.value()),
            }
            button {
                class: "btn",
                r#type: "button",
                disabled: edge_source.read().trim().is_empty()
                    || edge_target.read().trim().is_empty(),
                onclick: move |_| {
                    let source = edge_source.read().trim().to_string();
                    let target = edge_target.read().trim().to_string();
                    commit_ops(
                        vec![GraphOp::UpsertEdge(VaultEdge {
                            source: source.clone(),
                            target: target.clone(),
                        })],
                        format!("add edge {source} -> {target}"),
                    );
                    edge_source.set(String::new());
                    edge_target.set(String::new());
                },
                "Add edge"
            }
        }
        if let Some(sel) = &selected {
            div { class: "server",
                button {
                    class: "btn",
                    r#type: "button",
                    title: "Commit a DeleteNode for the canvas selection (edges are kept)",
                    onclick: {
                        let sel = sel.clone();
                        move |_| {
                            commit_ops(
                                vec![GraphOp::DeleteNode(NodeId(sel.clone()))],
                                format!("delete node {sel}"),
                            );
                        }
                    },
                    "Delete selected: {sel}"
                }
            }
        }
    }
}
