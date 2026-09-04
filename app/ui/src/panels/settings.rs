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

fn connection_panel(mut ctx: Ctx) -> Element {
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
            // Sessions view backend: empty = embedded single-user host (worlds
            // live in this browser); set = multi-user session-manager server.
            // The identity mirrors the `x-user` header the OIDC gateway
            // injects in cluster deployments.
            div { class: "server",
                input {
                    aria_label: "Session manager URL (empty = embedded)",
                    placeholder: "session manager URL (empty = embedded)",
                    value: "{ctx.sm_url}",
                    oninput: move |event| ctx.sm_url.set(event.value()),
                }
                button {
                    class: "btn",
                    r#type: "button",
                    onclick: move |_| {
                        api::set_session_manager_url(&ctx.sm_url.read());
                        // Host rebuild is driven by the identity below; poke it
                        // so a URL-only change also reconnects.
                        let u = ctx.user.read().clone();
                        ctx.user.set(u);
                    },
                    "Set"
                }
            }
            div { class: "server",
                input {
                    aria_label: "User name (x-user identity)",
                    placeholder: "user name (x-user identity)",
                    value: "{ctx.user}",
                    oninput: move |event| ctx.user.set(event.value()),
                }
                button {
                    class: "btn",
                    r#type: "button",
                    onclick: move |_| {
                        api::set_user_name(&ctx.user.read());
                        let u = ctx.user.read().clone();
                        ctx.user.set(u);
                    },
                    "Set"
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

/// The importer catalog for pure-frontend deployments (GitHub Pages): no
/// graph-api exists to answer `GET /importers`, so the tab describes the
/// importer running inside this browser — the CORS-only GitHub import —
/// instead of erroring on the static host's 404 page. Rendered when the
/// boot decision picked GitHub mode and the live graph is not server-backed
/// (a user who typed a reachable server URL into Connection gets the server
/// catalog back).
fn browser_importer_catalog() -> Element {
    // Live source first, then the persisted panel default, then the boot
    // spec (deep link or Pages default). Reading LAST_IMPORT subscribes the
    // tab so a completed import refreshes the card in place.
    let spec = crate::github::LAST_IMPORT
        .read()
        .clone()
        .or_else(crate::github::persisted_spec)
        .or_else(crate::github::boot_spec);
    rsx! {
        div {
            class: "importers-view",
            "data-activation": "browser",
            "data-runtime-switch": "browser",
            section { class: "importer-policy", role: "note",
                span { class: "importer-policy-label", "Browser-hosted" }
                p {
                    "No graph-api server: this browser lists, fetches, and parses the source \
                     itself (GitHub trees + raw endpoints, CORS-only). Public repositories \
                     only; indexed search and node metadata stay server features."
                }
            }
            if let Some(spec) = spec {
                section { class: "importer-active-summary",
                    div {
                        span { class: "importer-section-label", "Active importer" }
                        strong { "GitHub (in-browser)" }
                        code { "data-field": "active-kind", "github" }
                    }
                    dl { class: "importer-facts",
                        {importer_fact("package", "active-importer-id", &format!("github.{}", crate::github::source_id_for(&spec.repo)))}
                        {importer_fact("version", "active-importer-version", env!("CARGO_PKG_VERSION"))}
                        {importer_fact("repository", "selected-profile", &spec.repo)}
                        {importer_fact("ref", "active-ref", if spec.git_ref.is_empty() { "default branch" } else { &spec.git_ref })}
                        {importer_fact("path", "active-path", if spec.path.is_empty() { "(repository root)" } else { &spec.path })}
                        {importer_fact("namespace", "active-namespace", &format!("github:{}", crate::github::source_id_for(&spec.repo)))}
                    }
                    button {
                        class: "btn",
                        r#type: "button",
                        onclick: move |_| {
                            *crate::OPEN_PANEL.write() = Some(crate::Panel::GitHub);
                        },
                        "Configure in the GitHub panel"
                    }
                }
            } else {
                section { class: "importer-active-summary",
                    p { class: "importers-status", "Nothing imported yet." }
                    button {
                        class: "btn",
                        r#type: "button",
                        onclick: move |_| {
                            *crate::OPEN_PANEL.write() = Some(crate::Panel::GitHub);
                        },
                        "Open the GitHub panel"
                    }
                }
            }
        }
    }
}

fn importer_fact(label: &'static str, field: &'static str, value: &str) -> Element {
    rsx! {
        div { class: "importer-fact",
            dt { "{label}" }
            dd { "data-field": field, "{value}" }
        }
    }
}

/// One catalog card. The default collapsed view shows only what a viewer
/// needs to identify the source and switch to it: name, source kind,
/// identity (source id + filesystem rescan interval), description, and
/// one primary action button. Deployment-side configuration (volume,
/// claim, mount, producer contract) hides behind a `<details>` disclosure
/// so the default card stays compact while keeping the field selectors
/// the browser regression suite exercises intact.
#[allow(clippy::too_many_arguments)]
fn importer_card(
    profile: &api::ImporterProfile,
    switch_allowed: bool,
    viewing: Option<&str>,
    generation: Signal<u64>,
    mut on_switch: impl FnMut(Option<String>) + 'static,
) -> Element {
    let source_id = profile.source_id.as_deref().unwrap_or("—");
    let is_default = profile.selected;
    let is_viewing = match viewing {
        Some(id) => id == profile.id,
        None => is_default,
    };
    let selectable = is_default || profile.runnable;
    let has_details = profile.source.is_some() || profile.producer.is_some();
    // The browser regression's data-source-id / data-kind / data-default /
    // data-viewing selectors all key on attributes SelectableCard emits;
    // the shared `select-card` parent class only adds the neutral wrapper
    // so importers and engines share chrome. The `importer-card*` class
    // chain remains for the regression's
    // `[data-source-id="lavender-ingest-okf"]` selectors.
    let card_class: String = if is_viewing {
        "select-card importer-card importer-card-viewing".into()
    } else if is_default {
        "select-card importer-card importer-card-default".into()
    } else {
        "select-card importer-card".into()
    };
    // Chips render once, in the shared SelectableCard head strip — there
    // is no second badge row. The suffixes still carry the `importer-badge`
    // class so the browser regression's `.importer-badge.viewing` /
    // `.importer-badge.read-only` selectors keep matching.
    let mut chips: Vec<(String, String)> = Vec::new();
    chips.push((
        "importer-badge importer-kind-tag".to_string(),
        profile.kind.clone(),
    ));
    if is_viewing {
        chips.push(("importer-badge viewing".to_string(), "viewing".to_string()));
    }
    if profile
        .source
        .as_ref()
        .is_some_and(|source| source.read_only)
    {
        chips.push((
            "importer-badge read-only".to_string(),
            "read-only".to_string(),
        ));
    }
    let button_class: String = if is_viewing {
        "btn importer-switch-btn importer-switch-current".into()
    } else if is_default {
        "btn importer-switch-btn importer-switch-default".into()
    } else {
        "btn importer-switch-btn importer-switch-primary".into()
    };
    let button_label: &'static str = if is_viewing {
        if is_default { "Default · viewing" } else { "Viewing this source" }
    } else if is_default {
        "Return to default"
    } else {
        "View this source"
    };
    let switch_id = profile.id.clone();
    // The card is a <button>; the source editor sits beside it in the same
    // grid slot so its textarea/inputs are never nested inside a button.
    let has_definition = profile.kind == "httpjson";
    let definition_id = profile.id.clone();
    rsx! {
        div { class: "importer-slot",
        crate::selection_card::SelectableCard {
            source_id: profile.id.clone(),
            kind: profile.kind.clone(),
            card_class,
            name: profile.display_name.clone(),
            subtitle: profile.id.clone(),
            chips,
            description: profile.description.clone(),
            disabled_reason: String::new(),
            is_default,
            is_viewing,
            is_ghost: false,
            disabled: false,
            title: profile.id.clone(),
            on_select: None,
            dl { class: "importer-facts importer-identity",
                {importer_fact("source id", "source-id", source_id)}
                if let Some(interval) = profile.filesystem_rescan_interval_seconds {
                    {importer_fact(
                        "scan",
                        "filesystem-rescan",
                        &format!("{interval}s"),
                    )}
                }
            }
            if switch_allowed {
                div { class: "importer-switch-row",
                    button {
                        class: "{button_class}",
                        r#type: "button",
                        role: "radio",
                        aria_checked: if is_viewing { "true" } else { "false" },
                        "data-source-id": "{profile.id}",
                        "data-viewing": if is_viewing { "true" } else { "false" },
                        disabled: is_viewing || !selectable,
                        onclick: move |_| {
                            on_switch((!is_default).then(|| switch_id.clone()));
                        },
                        "{button_label}"
                    }
                }
            }
            if has_details {
                details { class: "importer-details",
                    summary { class: "importer-details-summary",
                        span { "Deployment details" }
                        span { class: "importer-details-hint", "filesystem + producer contract" }
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
        if has_definition {
            ImporterDefinitionEditor {
                source_id: definition_id,
                can_write: switch_allowed,
                generation,
            }
        }
        }
    }
}


/// Minimal Nix starter for a new package: one node collection with a
/// doctype rule, one declared field, the required schema block.
const NIX_PACKAGE_STARTER: &str = r#"# New HTTP/JSON importer package (Nix serialization).
# Evaluated server-side; the attrset must validate as an httpjson package.
let
  field = key: pointer: { inherit key pointer; };
in
{
  format_version = 1;
  metadata = {
    id = "example.items";
    name = "Example items";
    version = "0.1.0";
  };
  variables = [ { name = "project"; description = "Project path segment"; } ];
  collections = [
    {
      name = "items";
      path = "/v1/{project}/items";
      paginate = { style = "limit_offset"; };
      nodes = {
        id_pointer = "/id";
        node_type = "item";
        doctype = {
          pointer = "/kind";
          map = { task = "Task"; note = "Note"; };
        };
        title = { pointer = "/title"; fallback_prefix = "item"; };
        fields = [ (field "body" "/body") ];
        edges = [
          { kind = "references"; value_pointer = "/parent_id"; target_collection = "items"; match_on = "id"; }
        ];
      };
    }
  ];
  schema = {
    fields = [
      { key = "body"; field_type = "text"; required = false; searchable = true; facetable = false; snippet = true; }
    ];
    edge_types = [
      { key = "references"; directed = true; description = "Item -> the item it points at."; }
    ];
  };
}
"#;

#[derive(Clone, PartialEq)]
enum DefinitionState {
    Closed,
    Loading,
    Ready(api::ImporterDefinition),
    Failed(String),
}

/// Inline package-source editor for one httpjson card. The card keeps its
/// structured view; this reveals (and, when the server's packages dir is
/// writable and the viewer holds the switch group, edits) the authored
/// TOML/Nix underneath it.
#[component]
fn ImporterDefinitionEditor(
    source_id: String,
    can_write: bool,
    generation: Signal<u64>,
) -> Element {
    let mut state = use_signal(|| DefinitionState::Closed);
    let mut draft = use_signal(String::new);
    let mut status = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let open = !matches!(*state.read(), DefinitionState::Closed);

    let load_id = source_id.clone();
    let mut load = move || {
        let id = load_id.clone();
        state.set(DefinitionState::Loading);
        status.set(None);
        error.set(None);
        spawn(async move {
            match api::importer_definition(&id).await {
                Ok(definition) => {
                    draft.set(definition.source.clone());
                    state.set(DefinitionState::Ready(definition));
                }
                Err(message) => state.set(DefinitionState::Failed(message)),
            }
        });
    };

    let save_id = source_id.clone();
    let save = move |_| {
        let id = save_id.clone();
        let source = draft.read().clone();
        busy.set(true);
        status.set(Some("Validating and saving…".into()));
        error.set(None);
        spawn(async move {
            match api::put_importer_definition(&id, &source).await {
                Ok(definition) => {
                    draft.set(definition.source.clone());
                    state.set(DefinitionState::Ready(definition));
                    status.set(Some("Saved. The next switch to this source rebuilds it from the new package.".into()));
                    generation += 1;
                }
                Err(message) => {
                    status.set(None);
                    error.set(Some(message));
                }
            }
            busy.set(false);
        });
    };

    let view = state.read().clone();
    rsx! {
        div { class: "importer-definition", "data-source-id": "{source_id}",
            button {
                class: "btn importer-definition-toggle",
                r#type: "button",
                "aria-expanded": if open { "true" } else { "false" },
                onclick: move |_| {
                    if open { state.set(DefinitionState::Closed); } else { load(); }
                },
                if open { "Hide source" } else { "Edit source" }
            }
            match view {
                DefinitionState::Closed => rsx! {},
                DefinitionState::Loading => rsx! {
                    div { class: "gen-status", "Loading package definition…" }
                },
                DefinitionState::Failed(message) => rsx! {
                    div { class: "gen-error", "{message}" }
                },
                DefinitionState::Ready(definition) => {
                    let editable = can_write && definition.writable;
                    let dirty = *draft.read() != definition.source;
                    let original = definition.source.clone();
                    rsx! {
                        div { class: "importer-definition-head",
                            span { class: "importer-badge importer-format-badge", "data-format": "{definition.format}", "{definition.format}" }
                            code { class: "importer-definition-file", "{definition.package}" }
                        }
                        textarea {
                            class: "gen-editor importer-definition-editor",
                            rows: "18",
                            spellcheck: false,
                            readonly: !editable,
                            value: "{draft}",
                            oninput: move |e| draft.set(e.value()),
                        }
                        if !editable {
                            p { class: "importer-definition-note",
                                if !can_write {
                                    "Read-only: editing requires runtime switching to be enabled for your group."
                                } else {
                                    "Read-only: the server's packages directory is not writable."
                                }
                            }
                        }
                        div { class: "gen-actions",
                            button {
                                class: "btn importer-definition-save",
                                r#type: "button",
                                disabled: !editable || !dirty || *busy.read(),
                                onclick: save,
                                "Save"
                            }
                            button {
                                class: "btn importer-definition-cancel",
                                r#type: "button",
                                disabled: !dirty || *busy.read(),
                                onclick: move |_| {
                                    draft.set(original.clone());
                                    status.set(None);
                                    error.set(None);
                                },
                                "Cancel"
                            }
                        }
                        if let Some(message) = status.read().as_ref() {
                            div { class: "gen-status", role: "status", "{message}" }
                        }
                        if let Some(message) = error.read().as_ref() {
                            div { class: "gen-error", role: "alert", "{message}" }
                        }
                    }
                }
            }
        }
    }
}

/// Trailing card of the alternates list: a form that `POST`s a new runtime
/// httpjson source (catalog entry + package file) and refreshes the catalog.
#[component]
fn AddImporterCard(allowed: bool, generation: Signal<u64>) -> Element {
    let mut open = use_signal(|| false);
    let mut id = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut endpoint = use_signal(String::new);
    let mut package = use_signal(|| "my-importer.nix".to_string());
    let mut variables = use_signal(|| vec![(String::new(), String::new())]);
    let mut source = use_signal(|| NIX_PACKAGE_STARTER.to_string());
    let mut status = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    let submit = move |_| {
        let importer = api::NewImporter {
            id: id.read().trim().to_string(),
            name: name.read().trim().to_string(),
            description: String::new(),
            package: package.read().trim().to_string(),
            source: source.read().clone(),
            endpoint: endpoint.read().trim().to_string(),
            variables: variables
                .read()
                .iter()
                .filter(|(key, _)| !key.trim().is_empty())
                .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
                .collect(),
        };
        busy.set(true);
        status.set(Some("Validating and creating…".into()));
        error.set(None);
        spawn(async move {
            match api::post_importer(&importer).await {
                Ok(_) => {
                    status.set(None);
                    open.set(false);
                    id.set(String::new());
                    name.set(String::new());
                    endpoint.set(String::new());
                    variables.set(vec![(String::new(), String::new())]);
                    source.set(NIX_PACKAGE_STARTER.to_string());
                    generation += 1;
                }
                Err(message) => {
                    status.set(None);
                    error.set(Some(message));
                }
            }
            busy.set(false);
        });
    };

    let ready = !id.read().trim().is_empty()
        && !name.read().trim().is_empty()
        && !endpoint.read().trim().is_empty()
        && !package.read().trim().is_empty();
    let row_count = variables.read().len();
    rsx! {
        div { class: "select-card importer-card importer-add-card", "data-source-id": "__add__",
            div { class: "select-card-head",
                strong { "Add importer" }
            }
            if !allowed {
                p { class: "importer-definition-note",
                    "Adding a source requires runtime switching to be enabled for your group."
                }
            } else if !*open.read() {
                p { class: "select-card-desc",
                    "Declare a new HTTP/JSON source: a catalog entry plus its package, authored in Nix or TOML."
                }
                button {
                    class: "btn importer-add-open",
                    r#type: "button",
                    onclick: move |_| open.set(true),
                    "New importer…"
                }
            } else {
                div { class: "importer-add-form",
                    label { class: "importer-add-field",
                        span { "id" }
                        input { r#type: "text", placeholder: "my-bank", value: "{id}", oninput: move |e| id.set(e.value()) }
                    }
                    label { class: "importer-add-field",
                        span { "name" }
                        input { r#type: "text", placeholder: "My bank", value: "{name}", oninput: move |e| name.set(e.value()) }
                    }
                    label { class: "importer-add-field",
                        span { "endpoint" }
                        input { r#type: "text", placeholder: "http://api.example.svc:8080", value: "{endpoint}", oninput: move |e| endpoint.set(e.value()) }
                    }
                    label { class: "importer-add-field",
                        span { "package file" }
                        input { r#type: "text", placeholder: "my-importer.nix", value: "{package}", oninput: move |e| package.set(e.value()) }
                    }
                    div { class: "importer-add-variables",
                        span { class: "importer-add-label", "variables" }
                        for row in 0..row_count {
                            div { key: "{row}", class: "importer-add-variable",
                                input {
                                    r#type: "text",
                                    placeholder: "key",
                                    value: "{variables.read()[row].0}",
                                    oninput: move |e| variables.write()[row].0 = e.value(),
                                }
                                span { "=" }
                                input {
                                    r#type: "text",
                                    placeholder: "value",
                                    value: "{variables.read()[row].1}",
                                    oninput: move |e| variables.write()[row].1 = e.value(),
                                }
                                button {
                                    class: "btn importer-add-variable-remove",
                                    r#type: "button",
                                    title: "Remove variable",
                                    disabled: row_count == 1,
                                    onclick: move |_| { variables.write().remove(row); },
                                    "×"
                                }
                            }
                        }
                        button {
                            class: "btn importer-add-variable-add",
                            r#type: "button",
                            onclick: move |_| variables.write().push((String::new(), String::new())),
                            "+ variable"
                        }
                    }
                    span { class: "importer-add-label", "package source" }
                    textarea {
                        class: "gen-editor importer-definition-editor",
                        rows: "18",
                        spellcheck: false,
                        value: "{source}",
                        oninput: move |e| source.set(e.value()),
                    }
                    div { class: "gen-actions",
                        button {
                            class: "btn importer-add-submit",
                            r#type: "button",
                            disabled: !ready || *busy.read(),
                            onclick: submit,
                            "Create"
                        }
                        button {
                            class: "btn importer-add-cancel",
                            r#type: "button",
                            disabled: *busy.read(),
                            onclick: move |_| { open.set(false); error.set(None); status.set(None); },
                            "Cancel"
                        }
                    }
                    if let Some(message) = status.read().as_ref() {
                        div { class: "gen-status", role: "status", "{message}" }
                    }
                    if let Some(message) = error.read().as_ref() {
                        div { class: "gen-error", role: "alert", "{message}" }
                    }
                }
            }
        }
    }
}
fn importer_catalog(
    catalog: &api::ImporterCatalog,
    viewing: Option<String>,
    generation: Signal<u64>,
    mut on_switch: impl FnMut(Option<String>) + 'static + Copy,
) -> Element {
    let selected = catalog.selected.as_deref().unwrap_or("none");
    let active_kind = catalog.active.kind_label();
    let switch = &catalog.runtime_switch;
    let switch_state = if !switch.enabled {
        "disabled"
    } else if switch.allowed {
        "enabled"
    } else {
        "denied"
    };
    let switch_allowed = switch.enabled && switch.allowed;
    let required_group = switch.required_group.as_deref().unwrap_or("?");
    // A session override that's not the deployment default — whether stale
    // (denied because the viewer lost the required group, or the source was
    // undeployed) or simply an active non-default view — must always be
    // recoverable: the reset affordance returns to the deployment default,
    // which never requires authorization.
    let viewing_non_default = switch.enabled
        && viewing.is_some()
        && viewing.as_deref() != catalog.selected.as_deref();
    // Split profiles into the one the viewer is currently looking at
    // (prominent) and the alternatives they could switch to (compact list).
    // When the viewer has no session override, the deployment default is
    // the active view; otherwise the override is.
    let (viewing_profile, other_profiles): (Option<&api::ImporterProfile>, Vec<&api::ImporterProfile>) = {
        let viewing_id = viewing.as_deref();
        let active = catalog.sources.iter().find(|p| match viewing_id {
            Some(id) => p.id == id,
            None => p.selected,
        });
        let others: Vec<&api::ImporterProfile> = catalog
            .sources
            .iter()
            .filter(|p| match viewing_id {
                Some(id) => p.id != id,
                None => !p.selected,
            })
            .collect();
        (active, others)
    };
    rsx! {
        div {
            class: "importers-view",
            "data-activation": "{catalog.activation}",
            "data-runtime-switch": "{switch_state}",
            section { class: "importer-policy", role: "note",
                span { class: "importer-policy-label", "Deployment-managed" }
                if !switch.enabled {
                    p {
                        "Configured by Helm. A rollout is required to switch the active importer; "
                        "this view intentionally has no runtime activation controls."
                    }
                } else if switch.allowed {
                    p {
                        "Configured by Helm. Runtime viewing is enabled for your group: selecting a "
                        "source re-loads this browser session's graph as a read-only view; writes, "
                        "generation, and compute stay on the deployment default."
                    }
                } else {
                    p {
                        "Configured by Helm. Switching requires NetBird group "
                        code { "{required_group}" }
                        "."
                    }
                }
                if viewing_non_default {
                    button {
                        class: "btn importer-switch-reset",
                        r#type: "button",
                        onclick: move |_| on_switch(None),
                        "return to deployment default"
                    }
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
            // Currently-viewing: single prominent card so the viewer's eye
            // lands on it without scanning a list.
            if let Some(viewing_profile) = viewing_profile {
                section {
                    class: "importer-section importer-section-viewing",
                    div { class: "importer-section-label", "Currently viewing" }
                    div { class: "importer-list", "aria-label": "Active source",
                        {importer_card(viewing_profile, switch_allowed, viewing.as_deref(), generation, on_switch)}
                    }
                }
            }
            // Alternatives: compact list of every other runnable source. The
            // default sits here when the viewer is currently looking at an
            // override (return-target); alternates sit here otherwise. The
            // trailing card adds a new runtime httpjson source.
            section {
                class: "importer-section importer-section-alternates",
                div {
                    class: "importer-section-label",
                    if viewing_non_default { "Return to default" } else { "Other sources" }
                }
                div { class: "importer-list", "aria-label": "Other configured sources",
                    role: "radiogroup",
                    for profile in &other_profiles {
                        {importer_card(profile, switch_allowed, viewing.as_deref(), generation, on_switch)}
                    }
                    AddImporterCard { allowed: switch_allowed, generation }
                }
            }
        }
    }
}


#[allow(non_snake_case)]
fn ImportersSettings(props: DelegateProps) -> Element {
    let ctx = props.ctx;
    let mut state = use_signal(|| ImportersViewState::Loading);
    // Browser-hosted deployment (GitHub Pages or a ?gh= deep link): while no
    // server-backed graph is live there is no /importers endpoint — the
    // static host's 404 page is not JSON. Render the browser importer
    // catalog instead; connecting to a reachable server (Connection tab)
    // flips back to the server catalog automatically.
    let browser_hosted = crate::github::boot_spec().is_some();
    // The viewed source is session state (sessionStorage), not server state:
    // the catalog's `selected`/`active` always describe the deployment default.
    let mut viewing = use_signal(|| api::source_id());
    let mut generation = use_signal(|| 0_u64);
    use_effect(move || {
        // Re-run after every switch so the catalog's per-request posture
        // (`allowed`) and the cards' radio state refresh together.
        let _ = *generation.read();
        if browser_hosted && !ctx.graph_session.read().is_server_backed() {
            return;
        }
        spawn(async move {
            state.set(match api::importers().await {
                Ok(catalog) => ImportersViewState::Ready(catalog),
                Err(error) => ImportersViewState::Failed(error),
            });
        });
    });

    let on_switch = move |id: Option<String>| {
        match &id {
            Some(id) => api::set_source_id(id),
            None => api::clear_source_id(),
        }
        viewing.set(id);
        generation += 1;
        spawn(reload_graph(ctx));
    };

    if browser_hosted && !ctx.graph_session.read().is_server_backed() {
        return browser_importer_catalog();
    }

    let view = state.read().clone();
    match view {
        ImportersViewState::Loading => rsx! {
            div { class: "importers-status", role: "status", "Loading importer catalog…" }
        },
        ImportersViewState::Ready(catalog) => {
            importer_catalog(&catalog, viewing.read().clone(), generation, on_switch)
        }
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
                    SettingsTab::Importers => rsx! { ImportersSettings { ctx } },
                    SettingsTab::Layout => rsx! { LayoutSettings { ctx } },
                    SettingsTab::Appearance => rsx! { AppearanceSettings { ctx } },
                    SettingsTab::Camera => rsx! { CameraSettings { ctx } },
                }
            }
        }
    }
}
