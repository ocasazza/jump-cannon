//! Unified Settings surface.
//!
//! Connection and the read-only importer deployment catalog remain app-owned
//! while Layout, Appearance, and Camera delegate to their existing panel
//! modules. Each delegate is mounted through its own component scope because
//! those renderers use hooks whose ordering must not be coupled to the selected
//! tab.

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
    Importers,
    Layout,
    Appearance,
    Camera,
}

impl SettingsTab {
    const ALL: [Self; 5] = [
        Self::Connection,
        Self::Importers,
        Self::Layout,
        Self::Appearance,
        Self::Camera,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Importers => "Importers",
            Self::Layout => "Layout",
            Self::Appearance => "Appearance",
            Self::Camera => "Camera",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Importers => "importers",
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

/// The currently selected tab — lets app-level chrome (the panel header's
/// action slot) mirror tab-specific affordances, like the Layout tab's
/// backend switch.
pub(crate) fn active_tab() -> SettingsTab {
    *ACTIVE_TAB.read()
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

#[derive(Clone, Debug, PartialEq)]
enum ImportersViewState {
    Loading,
    Ready(api::ImporterCatalog),
    Failed(String),
}

fn importer_fact(label: &'static str, field: &'static str, value: &str) -> Element {
    rsx! {
        div { class: "importer-fact",
            dt { "{label}" }
            dd { "data-field": field, "{value}" }
        }
    }
}

fn importer_card(profile: &api::ImporterProfile) -> Element {
    let source_id = profile.source_id.as_deref().unwrap_or("—");
    rsx! {
        article {
            key: "{profile.id}",
            class: "importer-card",
            "data-source-id": "{profile.id}",
            "data-kind": "{profile.kind}",
            "data-selected": if profile.selected { "true" } else { "false" },
            "data-active": if profile.active { "true" } else { "false" },
            header { class: "importer-card-head",
                div {
                    h3 { "{profile.display_name}" }
                    code { class: "importer-profile-id", "{profile.id}" }
                }
                div { class: "importer-badges",
                    if profile.active {
                        span { class: "importer-badge active", "active" }
                    }
                    if profile.selected {
                        span { class: "importer-badge selected", "selected" }
                    }
                    if profile.source.as_ref().is_some_and(|source| source.read_only) {
                        span { class: "importer-badge read-only", "read-only" }
                    }
                }
            }
            if !profile.description.is_empty() {
                p { class: "importer-description", "{profile.description}" }
            }
            dl { class: "importer-facts importer-identity",
                {importer_fact("kind", "kind", &profile.kind)}
                {importer_fact("source id", "source-id", source_id)}
                if let Some(interval) = profile.filesystem_rescan_interval_seconds {
                    {importer_fact(
                        "filesystem rescan",
                        "filesystem-rescan",
                        &format!("{interval}s"),
                    )}
                }
            }
            if let Some(source) = &profile.source {
                section { class: "importer-contract importer-consumer",
                    h4 {
                        if source.read_only { "Read-only consumer" } else { "Filesystem source" }
                    }
                    dl { class: "importer-facts",
                        {importer_fact("volume", "consumer-volume", &source.volume_name)}
                        {importer_fact("claim", "consumer-claim", &source.existing_claim)}
                        {importer_fact("mount", "consumer-mount", &source.mount_path)}
                        {importer_fact("input", "consumer-input", &source.path)}
                        {importer_fact(
                            "access",
                            "consumer-access",
                            if source.read_only { "read-only" } else { "read-write" },
                        )}
                    }
                }
            }
            if let Some(producer) = &profile.producer {
                section { class: "importer-contract importer-producer",
                    h4 { "Producer contract" }
                    dl { class: "importer-facts",
                        {importer_fact("chart", "producer-chart", &producer.chart)}
                        {importer_fact(
                            "default claim",
                            "producer-default-claim",
                            &producer.default_claim,
                        )}
                        {importer_fact(
                            "repository root",
                            "producer-repository-root",
                            &producer.repository_root,
                        )}
                        {importer_fact(
                            "workflow input",
                            "producer-workflow-input",
                            &producer.workflow_input,
                        )}
                        {importer_fact(
                            "writer value",
                            "producer-existing-claim-value-path",
                            &producer.existing_claim_value_path,
                        )}
                        {importer_fact(
                            "writer claim",
                            "producer-existing-claim-value",
                            &producer.existing_claim_value,
                        )}
                    }
                }
            }
        }
    }
}

fn importer_catalog(catalog: &api::ImporterCatalog) -> Element {
    let selected = catalog.selected.as_deref().unwrap_or("none");
    let active_kind = catalog.active.kind_label();
    rsx! {
        div {
            class: "importers-view",
            "data-activation": "{catalog.activation}",
            section { class: "importer-policy", role: "note",
                span { class: "importer-policy-label", "Deployment-managed" }
                p {
                    "Configured by Helm. A rollout is required to switch the active importer; "
                    "this view intentionally has no runtime activation controls."
                }
            }
            section { class: "importer-active-summary",
                div {
                    span { class: "importer-section-label", "Active importer" }
                    strong { "{catalog.active.importer.name}" }
                    code { "data-field": "active-kind", "{active_kind}" }
                }
                dl { class: "importer-facts",
                    {importer_fact("package", "active-importer-id", &catalog.active.importer.id)}
                    {importer_fact("version", "active-importer-version", &catalog.active.importer.version)}
                    {importer_fact("selected profile", "selected-profile", selected)}
                }
            }
            div { class: "importer-list", "aria-label": "Configured importer profiles",
                for profile in &catalog.sources {
                    {importer_card(profile)}
                }
            }
        }
    }
}

#[allow(non_snake_case)]
fn ImportersSettings() -> Element {
    let mut state = use_signal(|| ImportersViewState::Loading);
    use_effect(move || {
        spawn(async move {
            state.set(match api::importers().await {
                Ok(catalog) => ImportersViewState::Ready(catalog),
                Err(error) => ImportersViewState::Failed(error),
            });
        });
    });

    let view = state.read().clone();
    match view {
        ImportersViewState::Loading => rsx! {
            div { class: "importers-status", role: "status", "Loading importer catalog…" }
        },
        ImportersViewState::Ready(catalog) => importer_catalog(&catalog),
        ImportersViewState::Failed(error) => rsx! {
            div { class: "importers-status error", role: "alert",
                "Importer catalog unavailable: {error}"
            }
        },
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
                    SettingsTab::Importers => rsx! { ImportersSettings {} },
                    SettingsTab::Layout => rsx! { LayoutSettings { ctx } },
                    SettingsTab::Appearance => rsx! { AppearanceSettings { ctx } },
                    SettingsTab::Camera => rsx! { CameraSettings { ctx } },
                }
            }
        }
    }
}
