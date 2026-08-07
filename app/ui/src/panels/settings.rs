//! Unified Settings surface.
//!
//! Connection remains app-owned while Layout, Appearance, and Camera delegate
//! to their existing panel modules. Each delegate is mounted through its own
//! component scope because those renderers use hooks whose ordering must not be
//! coupled to the selected tab.

use dioxus::events::{Key, KeyboardEvent};
use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::{api, reload_graph, Ctx};

const STORE_KEY: &str = "jc_settings_tab_v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingsTab {
    #[default]
    Connection,
    Layout,
    Appearance,
    Camera,
}

impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Connection,
        Self::Layout,
        Self::Appearance,
        Self::Camera,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Layout => "Layout",
            Self::Appearance => "Appearance",
            Self::Camera => "Camera",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Layout => "layout",
            Self::Appearance => "appearance",
            Self::Camera => "camera",
        }
    }

    fn tab_id(self) -> String {
        format!("settings-tab-{}", self.slug())
    }

    fn panel_id(self) -> String {
        format!("settings-panel-{}", self.slug())
    }

    fn adjacent(self, delta: isize) -> Self {
        let current = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0) as isize;
        let len = Self::ALL.len() as isize;
        Self::ALL[(current + delta).rem_euclid(len) as usize]
    }
}

static ACTIVE_TAB: GlobalSignal<SettingsTab> =
    Signal::global(|| LocalStorage::get(STORE_KEY).unwrap_or_default());

/// Select a Settings section from another app-owned surface, such as the
/// command palette. The workspace caller remains responsible for opening the
/// Settings panel itself.
pub(crate) fn select_tab(tab: SettingsTab) {
    *ACTIVE_TAB.write() = tab;
    let _ = LocalStorage::set(STORE_KEY, tab);
}

fn focus_tab(tab: SettingsTab) {
    let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&tab.tab_id()))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = element.focus();
}

fn activate_and_focus(tab: SettingsTab) {
    select_tab(tab);
    focus_tab(tab);
}

fn handle_tab_key(event: KeyboardEvent, tab: SettingsTab) {
    let target = match event.key() {
        Key::ArrowLeft => Some(tab.adjacent(-1)),
        Key::ArrowRight => Some(tab.adjacent(1)),
        Key::Home => Some(SettingsTab::ALL[0]),
        Key::End => Some(SettingsTab::ALL[SettingsTab::ALL.len() - 1]),
        _ => None,
    };

    if let Some(target) = target {
        event.prevent_default();
        activate_and_focus(target);
    }
}

fn tab_button(tab: SettingsTab, active: SettingsTab) -> Element {
    let selected = tab == active;
    let aria_selected = if selected { "true" } else { "false" };
    let tabindex = if selected { "0" } else { "-1" };

    rsx! {
        button {
            key: "{tab.slug()}",
            id: tab.tab_id(),
            class: "settings-tab",
            r#type: "button",
            role: "tab",
            aria_selected,
            aria_controls: tab.panel_id(),
            tabindex,
            onclick: move |_| activate_and_focus(tab),
            onkeydown: move |event: KeyboardEvent| handle_tab_key(event, tab),
            {tab.label()}
        }
    }
}

fn connection_panel(ctx: Ctx) -> Element {
    let Ctx {
        mut server,
        graph,
        graph_session,
        ..
    } = ctx;
    let graph = graph.read().clone();
    let session = graph_session.read().clone();

    rsx! {
        div { class: "controls",
            div { class: "server",
                input {
                    aria_label: "Graph API server URL",
                    value: "{server}",
                    oninput: move |event| server.set(event.value()),
                }
                button {
                    class: "btn",
                    r#type: "button",
                    onclick: move |_| {
                        api::set_server_url(&server.read());
                        spawn(reload_graph(ctx));
                    },
                    "Connect"
                }
            }
            if let Some(graph) = graph {
                div { class: "stats",
                    div { class: "kv",
                        span { class: "k", "nodes" }
                        span { class: "v", "{graph.n_nodes}" }
                    }
                    div { class: "kv",
                        span { class: "k", "edges" }
                        span { class: "v", "{graph.n_edges}" }
                    }
                    div { class: "kv",
                        span { class: "k", "communities" }
                        span { class: "v", "{graph.num_communities}" }
                    }
                    div { class: "kv",
                        span { class: "k", "components" }
                        span { class: "v", "{graph.num_wcc}" }
                    }
                }
            }
            div { class: "note", "active: {session.short_label()}" }
            div { class: "note",
                if session.is_server_backed() {
                    "Graph metadata/search/documents are served by graph-api. Compute-worker \
                     layouts are accepted only for this graph revision."
                } else {
                    "This generated graph is browser-owned. Metadata/search/documents and \
                     compute-worker layouts are disabled until it is hosted by graph-api."
                }
            }
        }
    }
}

/// Manual props keep `Ctx`'s signal-bundle type unchanged while still giving
/// each delegated renderer a real component boundary for its hooks.
#[derive(Clone, Copy, Props)]
struct DelegateProps {
    ctx: Ctx,
}

impl PartialEq for DelegateProps {
    fn eq(&self, other: &Self) -> bool {
        self.ctx.graph == other.ctx.graph
            && self.ctx.graph_session == other.ctx.graph_session
            && self.ctx.load_error == other.ctx.load_error
            && self.ctx.selected == other.ctx.selected
            && self.ctx.meta == other.ctx.meta
            && self.ctx.meta_busy == other.ctx.meta_busy
            && self.ctx.draft == other.ctx.draft
            && self.ctx.save_msg == other.ctx.save_msg
            && self.ctx.query == other.ctx.query
            && self.ctx.results == other.ctx.results
            && self.ctx.result_total == other.ctx.result_total
            && self.ctx.searching == other.ctx.searching
            && self.ctx.server == other.ctx.server
            && self.ctx.tasks == other.ctx.tasks
            && self.ctx.logs == other.ctx.logs
    }
}

#[allow(non_snake_case)]
fn LayoutSettings(props: DelegateProps) -> Element {
    super::layout::panel(props.ctx)
}

#[allow(non_snake_case)]
fn AppearanceSettings(props: DelegateProps) -> Element {
    super::style::panel(props.ctx)
}

#[allow(non_snake_case)]
fn CameraSettings(props: DelegateProps) -> Element {
    super::camera::panel(props.ctx)
}

pub fn panel(ctx: Ctx) -> Element {
    let active = *ACTIVE_TAB.read();

    rsx! {
        div { class: "settings-shell",
            div {
                class: "settings-tabs",
                role: "tablist",
                aria_label: "Settings sections",
                aria_orientation: "horizontal",
                for tab in SettingsTab::ALL {
                    {tab_button(tab, active)}
                }
            }
            div {
                class: "settings-tabpanel",
                id: active.panel_id(),
                role: "tabpanel",
                aria_labelledby: active.tab_id(),
                tabindex: "0",
                match active {
                    SettingsTab::Connection => connection_panel(ctx),
                    SettingsTab::Layout => rsx! { LayoutSettings { ctx } },
                    SettingsTab::Appearance => rsx! { AppearanceSettings { ctx } },
                    SettingsTab::Camera => rsx! { CameraSettings { ctx } },
                }
            }
        }
    }
}
