//! Ctrl/⌘+P command palette + action registry — Dioxus port of
//! `crates/graph-renderer/src/ui/command_palette/{mod,matching}.rs` and
//! `crates/graph-renderer/src/ui/actions/{mod,builtins}.rs` at 723af10.
//!
//! Two render modes, same as the egui original:
//!   * search/list mode — fuzzy-matched action list with breadcrumb +
//!     category grouping, fzf-style vault-file matches with a metadata
//!     preview pane; arrow-key navigation, Enter to descend or execute,
//!     Tab to complete, Backspace (empty query) to pop the breadcrumb.
//!   * parameter form mode — driven by `ActionRegistry::configuring`,
//!     walks one parameter at a time with per-param validation.
//!
//! The egui app bubbled a `PaletteOutcome` up to `App::execute_action`;
//! here the dispatch (`run_builtin`) lives in this module and reaches its
//! targets through the other modules' pub surfaces (`render::fit_camera`,
//! `panels::filter::edit_filters`, …). Workspace settings (font size /
//! family / line numbers — the egui `WorkspaceSettings`) are owned here
//! and persisted under `jc_workspace_v1`.
//!
//! PARITY GAPs (see `parity_gap` and the per-arm comments in
//! `run_builtin`): Reset Style, New Graph Tab and the Go-to-section
//! actions are registered for palette parity but disabled — their targets
//! (panels::style's private state, the panel-kit workspace handle inside
//! main.rs::use_workspace) are unreachable from this module.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use dioxus::events::{Key, KeyboardEvent, Modifiers, MountedEvent};
use dioxus::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use serde::{Deserialize, Serialize};

use crate::panels::filter::{self, Card, ConnectorOp, Op};
use crate::{api, proto, render, Ctx};

// --- action data model (port of actions/mod.rs) ----------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionType {
    Singleton,
    MultiInstance,
}

impl ActionType {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ActionType::Singleton => "Singleton",
            ActionType::MultiInstance => "Multi-instance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParameterType {
    String,
    Number,
    Boolean,
    Select,
    MultiSelect,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ParameterValidation {
    pub pattern: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParameterOption {
    pub value: String,
    pub label: String,
}

/// Type-safe parameter value. Select stores a 1-element vec so Select and
/// MultiSelect share a widget contract (same rationale as the egui port).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum ParamValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Selected(Vec<String>),
}

impl ParamValue {
    pub(crate) fn default_for(ty: ParameterType) -> Self {
        match ty {
            ParameterType::String => ParamValue::String(String::new()),
            ParameterType::Number => ParamValue::Number(0.0),
            ParameterType::Boolean => ParamValue::Boolean(false),
            ParameterType::Select | ParameterType::MultiSelect => {
                ParamValue::Selected(Vec::new())
            }
        }
    }

    pub(crate) fn as_string(&self) -> Option<&str> {
        if let ParamValue::String(s) = self { Some(s) } else { None }
    }
    pub(crate) fn as_selected(&self) -> Option<&[String]> {
        if let ParamValue::Selected(v) = self { Some(v.as_slice()) } else { None }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActionParameter {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ParameterType,
    pub required: bool,
    pub default: Option<ParamValue>,
    pub options: Vec<ParameterOption>,
    pub validation: ParameterValidation,
}

/// The egui sidebar's `Section` enum — carried by `JumpToSection` so the
/// action ids/titles match the original ("jump-filter", "Go to Filter", …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Filter,
    Style,
    Layout,
    Camera,
    Instances,
    Debug,
    Metrics,
    Generate,
    Timeline,
}

impl Section {
    pub(crate) const ALL: &'static [Section] = &[
        Section::Filter,
        Section::Style,
        Section::Layout,
        Section::Camera,
        Section::Instances,
        Section::Debug,
        Section::Metrics,
        Section::Generate,
        Section::Timeline,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Section::Filter => "Filter",
            Section::Style => "Style",
            Section::Layout => "Layout",
            Section::Camera => "Camera",
            Section::Instances => "Instances",
            Section::Debug => "Debug",
            Section::Metrics => "Metrics",
            Section::Generate => "Generate (tvix)",
            Section::Timeline => "Timeline",
        }
    }

    /// The workspace panel this section opens — egui's
    /// `set_section_open(sec, true)` target, as a `Panel` for
    /// `main.rs::OPEN_PANEL`.
    fn panel(self) -> crate::Panel {
        match self {
            Section::Filter => crate::Panel::Filter,
            Section::Style => crate::Panel::Style,
            Section::Layout => crate::Panel::Layout,
            Section::Camera => crate::Panel::Camera,
            Section::Instances => crate::Panel::Instances,
            Section::Debug => crate::Panel::Debug,
            Section::Metrics => crate::Panel::Metrics,
            Section::Generate => crate::Panel::Generate,
            Section::Timeline => crate::Panel::Timeline,
        }
    }
}

/// Built-in action handlers. Enum-based dispatch (vs. boxed closures)
/// keeps actions inspectable — same shape as the egui port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinAction {
    // Settings tree
    Settings,
    EditOptions,
    FontSize,
    FontFamily,
    LineNumbers,
    ToggleTheme,
    // Node Operations tree
    NodeOperations,
    Filter,
    FilterByName,
    FilterByContent,
    FilterByTag,
    SearchNodes,
    CreateNode,
    // Direct view actions
    FitCamera,
    ResetStyle,
    JumpToSection(Section),
    NewGraphTab,
}

#[derive(Debug, Clone)]
pub(crate) enum ActionHandler {
    Builtin(BuiltinAction),
}

#[derive(Debug, Clone)]
pub(crate) struct Action {
    pub id: String,
    pub title: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub kind: ActionType,
    pub parameters: Vec<ActionParameter>,
    pub parent_id: Option<String>,
    pub children_ids: Vec<String>,
    pub category: Option<String>,
    /// Display-only keyboard shortcut hint (e.g. "F") — the actual binding
    /// lives in main.rs's onkeydown.
    pub shortcut: Option<String>,
    pub handler: ActionHandler,
}

/// One recorded execution. Session-only, like the egui registry (it was
/// never part of the persisted AppState).
// Consumer is the Instances panel's actions_section (currently a stub) —
// see `instances_snapshot` / `remove_instance` below.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ActionInstance {
    pub id: u64,
    pub action_id: String,
    /// Free-form output from the handler; JSON for heterogeneity.
    pub state: serde_json::Value,
    pub params: HashMap<String, ParamValue>,
}

/// In-progress parameter form. `None` when not configuring. Validation
/// errors are computed render-time by `check_param` (same visible behavior
/// as the egui per-frame `validate_param`), so no error map is stored.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfiguringState {
    pub action_id: String,
    pub current_param_index: usize,
    pub form_values: HashMap<String, ParamValue>,
}

#[derive(Default)]
pub(crate) struct ActionRegistry {
    pub actions: Vec<Action>,
    pub instances: Vec<ActionInstance>,
    pub configuring: Option<ConfiguringState>,
    pub next_instance_id: u64,
}

impl ActionRegistry {
    pub(crate) fn register(&mut self, action: Action) {
        if let Some(existing) = self.actions.iter_mut().find(|a| a.id == action.id) {
            *existing = action;
        } else {
            self.actions.push(action);
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub(crate) fn root_actions(&self) -> Vec<&Action> {
        self.actions.iter().filter(|a| a.parent_id.is_none()).collect()
    }

    pub(crate) fn child_actions(&self, parent_id: &str) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| a.parent_id.as_deref() == Some(parent_id))
            .collect()
    }

    /// Begin a parameter form for `action_id`, pre-filling from
    /// `initial_values` (smart defaults), each parameter's `default`, or
    /// the type default — same precedence as the egui registry.
    pub(crate) fn start_configuring(
        &mut self,
        action_id: &str,
        initial_values: &HashMap<String, ParamValue>,
    ) {
        let Some(action) = self.get(action_id).cloned() else { return };
        let mut form_values: HashMap<String, ParamValue> = HashMap::new();
        for p in &action.parameters {
            let v = initial_values
                .get(&p.id)
                .cloned()
                .or_else(|| p.default.clone())
                .unwrap_or_else(|| ParamValue::default_for(p.kind));
            form_values.insert(p.id.clone(), v);
        }
        self.configuring = Some(ConfiguringState {
            action_id: action_id.to_string(),
            current_param_index: 0,
            form_values,
        });
    }

    pub(crate) fn cancel_configuring(&mut self) {
        self.configuring = None;
    }

    /// Finalize the form — caller runs `execute_action` with the returned
    /// (action_id, params). Clears `configuring`.
    pub(crate) fn take_finished_form(&mut self) -> Option<(String, HashMap<String, ParamValue>)> {
        let cfg = self.configuring.take()?;
        Some((cfg.action_id, cfg.form_values))
    }

    /// Record an ActionInstance (or update the existing one for
    /// singletons). Returns the instance id.
    pub(crate) fn record_execution(
        &mut self,
        action_id: &str,
        params: HashMap<String, ParamValue>,
        state: serde_json::Value,
    ) -> u64 {
        let kind = self
            .get(action_id)
            .map(|a| a.kind)
            .unwrap_or(ActionType::MultiInstance);
        if kind == ActionType::Singleton {
            if let Some(existing) =
                self.instances.iter_mut().find(|i| i.action_id == action_id)
            {
                existing.params = params;
                existing.state = state;
                return existing.id;
            }
        }
        self.next_instance_id += 1;
        let id = self.next_instance_id;
        self.instances.push(ActionInstance {
            id,
            action_id: action_id.to_string(),
            state,
            params,
        });
        id
    }
}

/// Validate one parameter value — port of the egui `validate_param`, made
/// pure so the form can call it at render time without mutating state.
fn check_param(param: &ActionParameter, value: Option<&ParamValue>) -> Result<(), String> {
    if param.required {
        let empty = match value {
            None => true,
            Some(ParamValue::String(s)) => s.is_empty(),
            Some(ParamValue::Selected(v)) => v.is_empty(),
            _ => false,
        };
        if empty {
            return Err("This field is required".to_string());
        }
    }
    match (param.kind, value) {
        (ParameterType::Number, Some(ParamValue::Number(n))) => {
            if let Some(min) = param.validation.min {
                if *n < min {
                    return Err(format!("Minimum value is {min}"));
                }
            }
            if let Some(max) = param.validation.max {
                if *n > max {
                    return Err(format!("Maximum value is {max}"));
                }
            }
        }
        (ParameterType::String, Some(ParamValue::String(s))) => {
            // Cheap substring check, not full regex — egui parity.
            if let Some(pat) = &param.validation.pattern {
                if !pat.is_empty() && !s.contains(pat) {
                    return Err(format!("Should contain `{pat}`"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// --- builtin seed data (port of actions/builtins.rs) -----------------------------

fn words(ws: &[&str]) -> Vec<String> {
    ws.iter().map(|s| s.to_string()).collect()
}

fn font_family_options() -> Vec<ParameterOption> {
    vec![
        ParameterOption { value: "monospace".into(), label: "Monospace".into() },
        ParameterOption { value: "sans-serif".into(), label: "Sans Serif".into() },
        ParameterOption { value: "serif".into(), label: "Serif".into() },
    ]
}

fn seed_default_actions(reg: &mut ActionRegistry) {
    use BuiltinAction as B;

    let font_size_param = ActionParameter {
        id: "font_size".into(),
        name: "Font Size".into(),
        description: "Font size in pixels".into(),
        kind: ParameterType::Number,
        required: true,
        default: Some(ParamValue::Number(14.0)),
        options: vec![],
        validation: ParameterValidation { pattern: None, min: Some(8.0), max: Some(32.0) },
    };
    let font_family_param = ActionParameter {
        id: "font_family".into(),
        name: "Font Family".into(),
        description: "Font family for the editor".into(),
        kind: ParameterType::Select,
        required: true,
        default: Some(ParamValue::Selected(vec!["monospace".into()])),
        options: font_family_options(),
        validation: ParameterValidation::default(),
    };
    let line_numbers_param = |required: bool| ActionParameter {
        id: "show_line_numbers".into(),
        name: "Show Line Numbers".into(),
        description: "Display line numbers in the editor".into(),
        kind: ParameterType::Boolean,
        required,
        default: Some(ParamValue::Boolean(true)),
        options: vec![],
        validation: ParameterValidation::default(),
    };

    // ===== Settings =====
    reg.register(Action {
        id: "settings".into(),
        title: "Settings".into(),
        description: "Configure application settings".into(),
        keywords: words(&["settings", "options", "preferences", "configure"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![
            "edit-options".into(),
            "toggle-theme".into(),
            "font-size".into(),
            "font-family".into(),
            "line-numbers".into(),
        ],
        category: Some("System".into()),
        shortcut: None,
        handler: ActionHandler::Builtin(B::Settings),
    });

    reg.register(Action {
        id: "edit-options".into(),
        title: "Edit Options".into(),
        description: "Configure application settings".into(),
        keywords: words(&["settings", "options", "preferences", "configure"]),
        kind: ActionType::Singleton,
        parameters: vec![
            font_size_param.clone(),
            font_family_param.clone(),
            line_numbers_param(false),
        ],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::EditOptions),
    });

    reg.register(Action {
        id: "font-size".into(),
        title: "Change Font Size".into(),
        description: "Adjust the font size".into(),
        keywords: words(&["font", "size", "text", "zoom"]),
        kind: ActionType::Singleton,
        parameters: vec![font_size_param],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::FontSize),
    });

    reg.register(Action {
        id: "font-family".into(),
        title: "Change Font Family".into(),
        description: "Change the font family".into(),
        keywords: words(&["font", "family", "typeface"]),
        kind: ActionType::Singleton,
        parameters: vec![font_family_param],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::FontFamily),
    });

    reg.register(Action {
        id: "line-numbers".into(),
        title: "Toggle Line Numbers".into(),
        description: "Show or hide line numbers".into(),
        keywords: words(&["line", "numbers", "gutter"]),
        kind: ActionType::Singleton,
        parameters: vec![line_numbers_param(true)],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::LineNumbers),
    });

    reg.register(Action {
        id: "toggle-theme".into(),
        title: "Toggle Theme".into(),
        description: "Switch between light and dark themes".into(),
        keywords: words(&["theme", "dark", "light", "toggle", "switch"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::ToggleTheme),
    });

    // ===== Node Operations =====
    reg.register(Action {
        id: "node-operations".into(),
        title: "Node Operations".into(),
        description: "Operations for working with nodes".into(),
        keywords: words(&["node", "operations", "actions"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![
            "filter-actions".into(),
            "search-nodes".into(),
            "create-node".into(),
        ],
        category: Some("Nodes".into()),
        shortcut: None,
        handler: ActionHandler::Builtin(B::NodeOperations),
    });

    reg.register(Action {
        id: "filter-actions".into(),
        title: "Filter".into(),
        description: "Apply filters to nodes".into(),
        keywords: words(&["filter", "search", "find"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: Some("node-operations".into()),
        children_ids: vec![
            "filter-by-name".into(),
            "filter-by-content".into(),
            "filter-by-tag".into(),
        ],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::Filter),
    });

    let case_param = ActionParameter {
        id: "case_sensitive".into(),
        name: "Case Sensitive".into(),
        description: "Match case exactly".into(),
        kind: ParameterType::Boolean,
        required: false,
        default: Some(ParamValue::Boolean(false)),
        options: vec![],
        validation: ParameterValidation::default(),
    };

    reg.register(Action {
        id: "filter-by-name".into(),
        title: "Filter by Name".into(),
        description: "Filter nodes by name".into(),
        keywords: words(&["filter", "name"]),
        kind: ActionType::MultiInstance,
        parameters: vec![
            ActionParameter {
                id: "pattern".into(),
                name: "Name Pattern".into(),
                description: "Pattern to match node names".into(),
                kind: ParameterType::String,
                required: true,
                default: Some(ParamValue::String("*".into())),
                options: vec![],
                validation: ParameterValidation::default(),
            },
            case_param.clone(),
        ],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::FilterByName),
    });

    reg.register(Action {
        id: "filter-by-content".into(),
        title: "Filter by Content".into(),
        description: "Filter nodes by content".into(),
        keywords: words(&["filter", "content"]),
        kind: ActionType::MultiInstance,
        parameters: vec![
            ActionParameter {
                id: "pattern".into(),
                name: "Content Pattern".into(),
                description: "Pattern to match node content".into(),
                kind: ParameterType::String,
                required: true,
                default: Some(ParamValue::String(String::new())),
                options: vec![],
                validation: ParameterValidation::default(),
            },
            case_param,
        ],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::FilterByContent),
    });

    reg.register(Action {
        id: "filter-by-tag".into(),
        title: "Filter by Tag".into(),
        description: "Filter nodes by tag".into(),
        keywords: words(&["filter", "tag"]),
        kind: ActionType::MultiInstance,
        parameters: vec![ActionParameter {
            id: "tags".into(),
            name: "Tags".into(),
            description: "Tags to filter by".into(),
            kind: ParameterType::MultiSelect,
            required: true,
            default: Some(ParamValue::Selected(Vec::new())),
            // Seeded empty — populated dynamically once a tags-list
            // pipeline is plumbed in (egui parity: also empty there).
            options: vec![],
            validation: ParameterValidation::default(),
        }],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::FilterByTag),
    });

    reg.register(Action {
        id: "search-nodes".into(),
        title: "Search Nodes".into(),
        description: "Search for nodes by name or content".into(),
        keywords: words(&["search", "find", "nodes", "query"]),
        kind: ActionType::MultiInstance,
        parameters: vec![
            ActionParameter {
                id: "query".into(),
                name: "Search Query".into(),
                description: "Text to search for".into(),
                kind: ParameterType::String,
                required: true,
                default: Some(ParamValue::String(String::new())),
                options: vec![],
                validation: ParameterValidation::default(),
            },
            ActionParameter {
                id: "scope".into(),
                name: "Search Scope".into(),
                description: "Where to search".into(),
                kind: ParameterType::Select,
                required: true,
                default: Some(ParamValue::Selected(vec!["all".into()])),
                options: vec![
                    ParameterOption { value: "all".into(), label: "All Nodes".into() },
                    ParameterOption { value: "selected".into(), label: "Selected Nodes".into() },
                    ParameterOption { value: "visible".into(), label: "Visible Nodes".into() },
                ],
                validation: ParameterValidation::default(),
            },
            ActionParameter {
                id: "include_content".into(),
                name: "Include Content".into(),
                description: "Search in node content".into(),
                kind: ParameterType::Boolean,
                required: false,
                default: Some(ParamValue::Boolean(true)),
                options: vec![],
                validation: ParameterValidation::default(),
            },
        ],
        parent_id: Some("node-operations".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::SearchNodes),
    });

    reg.register(Action {
        id: "create-node".into(),
        title: "Create New Node".into(),
        description: "Create a new node in the workspace".into(),
        keywords: words(&["create", "new", "node", "add"]),
        kind: ActionType::MultiInstance,
        parameters: vec![
            ActionParameter {
                id: "name".into(),
                name: "Node Name".into(),
                description: "Name of the new node".into(),
                kind: ParameterType::String,
                required: true,
                default: Some(ParamValue::String("New Node".into())),
                options: vec![],
                validation: ParameterValidation::default(),
            },
            ActionParameter {
                id: "node_type".into(),
                name: "Node Type".into(),
                description: "Type of node to create".into(),
                kind: ParameterType::Select,
                required: true,
                default: Some(ParamValue::Selected(vec!["default".into()])),
                options: vec![
                    ParameterOption { value: "default".into(), label: "Default".into() },
                    ParameterOption { value: "text".into(), label: "Text".into() },
                    ParameterOption { value: "image".into(), label: "Image".into() },
                    ParameterOption { value: "code".into(), label: "Code".into() },
                ],
                validation: ParameterValidation::default(),
            },
        ],
        parent_id: Some("node-operations".into()),
        children_ids: vec![],
        category: None,
        shortcut: None,
        handler: ActionHandler::Builtin(B::CreateNode),
    });

    // ===== Direct view actions (parameterless, top-level) =====
    reg.register(Action {
        id: "fit-camera".into(),
        title: "Fit Camera".into(),
        description: "Fit the camera to the loaded graph bounds".into(),
        keywords: words(&["camera", "fit", "zoom", "view"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![],
        category: Some("View".into()),
        shortcut: Some("F".into()),
        handler: ActionHandler::Builtin(B::FitCamera),
    });

    // PARITY GAP: panel-kit hosts a single Graph panel — there is no
    // egui_dock-style tab strip to push a second Graph tab into.
    reg.register(Action {
        id: "new-graph-tab".into(),
        title: "New Graph Tab".into(),
        description: "Open a new Graph tab in the central workspace".into(),
        keywords: words(&["new", "tab", "graph", "open", "workspace", "dock"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![],
        category: Some("View".into()),
        shortcut: None,
        handler: ActionHandler::Builtin(B::NewGraphTab),
    });

    // PARITY GAP: the live style state + its persist path are private to
    // panels::style (only `panel()` is pub) — needs a pub(crate) reset hook.
    reg.register(Action {
        id: "reset-style".into(),
        title: "Reset Style".into(),
        description: "Reset all node and edge style settings to defaults".into(),
        keywords: words(&["reset", "style", "defaults", "clear"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![],
        category: Some("View".into()),
        shortcut: None,
        handler: ActionHandler::Builtin(B::ResetStyle),
    });

    // PARITY GAP: panel open/restore state lives in main.rs's
    // `use_workspace` hook — not reachable from a module-level dispatch.
    for &section in Section::ALL {
        let id = format!("jump-{}", section.title().to_lowercase());
        let title = format!("Go to {}", section.title());
        reg.register(Action {
            id,
            title,
            description: format!("Open the {} sidebar section", section.title()),
            keywords: words(&["go", "jump", "open", section.title()]),
            kind: ActionType::Singleton,
            parameters: vec![],
            parent_id: None,
            children_ids: vec![],
            category: Some("Navigation".into()),
            shortcut: None,
            handler: ActionHandler::Builtin(B::JumpToSection(section)),
        });
    }
}

/// Reason an action is registered-but-disabled, or `None` when it is fully
/// wired. Disabled rows render dimmed (reason in the tooltip) and are inert.
fn parity_gap(action_id: &str) -> Option<&'static str> {
    match action_id {
        "new-graph-tab" => Some("unavailable: panel-kit hosts a single Graph panel"),
        _ => None,
    }
}

// --- workspace settings (port of ui/state.rs::WorkspaceSettings) -----------------

const WS_KEY: &str = "jc_workspace_v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FontFamilyChoice {
    #[default]
    Monospace,
    SansSerif,
    Serif,
}

impl FontFamilyChoice {
    fn as_value(self) -> &'static str {
        match self {
            FontFamilyChoice::Monospace => "monospace",
            FontFamilyChoice::SansSerif => "sans-serif",
            FontFamilyChoice::Serif => "serif",
        }
    }
}

fn parse_font_family(s: &str) -> FontFamilyChoice {
    match s {
        "sans-serif" => FontFamilyChoice::SansSerif,
        "serif" => FontFamilyChoice::Serif,
        _ => FontFamilyChoice::Monospace,
    }
}

/// Settings driven by the palette's Settings sub-tree. The egui app stored
/// these on `AppState` (the document viewer consumed them); here the
/// palette owns them — a Document-panel consumer can read this signal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceSettings {
    pub font_size: f32,
    pub font_family: FontFamilyChoice,
    pub show_line_numbers: bool,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_family: FontFamilyChoice::default(),
            show_line_numbers: true,
        }
    }
}

pub(crate) static WORKSPACE: GlobalSignal<WorkspaceSettings> =
    Signal::global(|| LocalStorage::get(WS_KEY).unwrap_or_default());

fn update_workspace(f: impl FnOnce(&mut WorkspaceSettings)) {
    let snap = {
        let mut w = WORKSPACE.write();
        f(&mut w);
        *w
    };
    let _ = LocalStorage::set(WS_KEY, &snap);
}

/// Smart defaults for the Settings sub-tree's parameter forms — port of
/// the egui `workspace_initial_for`.
fn workspace_initial_for(action: &Action) -> HashMap<String, ParamValue> {
    let ws = *WORKSPACE.peek();
    let mut m = HashMap::new();
    for p in &action.parameters {
        match p.id.as_str() {
            "font_size" => {
                m.insert(p.id.clone(), ParamValue::Number(ws.font_size as f64));
            }
            "font_family" => {
                m.insert(
                    p.id.clone(),
                    ParamValue::Selected(vec![ws.font_family.as_value().to_string()]),
                );
            }
            "show_line_numbers" => {
                m.insert(p.id.clone(), ParamValue::Boolean(ws.show_line_numbers));
            }
            _ => {}
        }
    }
    m
}

// --- matching (port of command_palette/matching.rs) -------------------------------

/// Maximum number of file/node matches surfaced under the action list.
const FILE_MATCH_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
struct MatchInfo {
    score: i32,
    /// Byte indices in `title` that matched (used for highlighting).
    title_hits: Vec<usize>,
}

/// Case-insensitive subsequence match across title + description + keywords.
/// Returns `None` if every character in the query couldn't be placed.
/// Score rewards consecutive matches and earlier positions.
fn fuzzy_score(action: &Action, query: &str) -> Option<MatchInfo> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Some(MatchInfo::default());
    }
    // Title is the primary haystack; description + keywords broaden hits.
    let title_lc = action.title.to_lowercase();
    let mut title_hits: Vec<usize> = Vec::new();
    let title_score = subsequence_score(&title_lc, &q, Some(&mut title_hits));

    let extras = format!(
        "{} {} {}",
        action.description.to_lowercase(),
        action.keywords.join(" ").to_lowercase(),
        action.id.to_lowercase()
    );
    let extra_score = subsequence_score(&extras, &q, None);

    if title_score.is_none() && extra_score.is_none() {
        return None;
    }
    let title = title_score.unwrap_or(0);
    let extra = extra_score.unwrap_or(0);
    Some(MatchInfo {
        // Title matches weighted 3x — name typing should dominate.
        score: title * 3 + extra,
        title_hits,
    })
}

fn subsequence_score(
    haystack: &str,
    needle: &str,
    mut hits: Option<&mut Vec<usize>>,
) -> Option<i32> {
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut hi = 0;
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    let mut ni = 0;
    while ni < n_bytes.len() {
        let nb = n_bytes[ni];
        let mut found = None;
        while hi < h_bytes.len() {
            if h_bytes[hi] == nb {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let pos = found?;
        if let Some(h) = hits.as_deref_mut() {
            h.push(pos);
        }
        // Reward consecutive hits, prefer earlier matches.
        if Some(pos.wrapping_sub(1)) == last_match {
            score += 5;
        } else {
            score += 1;
        }
        if pos < 8 {
            score += 2;
        }
        last_match = Some(pos);
        ni += 1;
    }
    Some(score)
}

/// One ranked vault node + the byte indices in its id that matched.
#[derive(Debug, Clone)]
struct FileMatch {
    id: String,
    score: i32,
    indices: Vec<usize>,
}

/// fzf-style scorer with match positions — the same matcher the Nodes
/// browser uses (`panels/nodes.rs::fuzzy_match`), standing in for the egui
/// palette's SkimMatcherV2 (fuzzy_matcher is not a dependency here).
fn fuzzy_file(needle: &str, hay: &str) -> Option<(i32, Vec<usize>)> {
    let hay_lower = hay.to_lowercase();
    let hay_bytes = hay_lower.as_bytes();
    let mut score = 0i32;
    let mut positions = Vec::with_capacity(needle.len());
    let mut hi = 0usize;
    let mut prev_match: Option<usize> = None;
    for nc in needle.bytes() {
        let mut found = None;
        while hi < hay_bytes.len() {
            if hay_bytes[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let pos = found?;
        score += 2;
        if prev_match == Some(pos.wrapping_sub(1)) {
            score += 3; // consecutive run
        }
        if pos == 0 || matches!(hay_bytes[pos - 1], b'/' | b'-' | b'_' | b' ' | b'.') {
            score += 2; // segment start
        }
        positions.push(pos);
        prev_match = Some(pos);
        hi = pos + 1;
    }
    // Shorter haystacks win ties — exacter matches first.
    score -= (hay.len() / 16) as i32;
    Some((score, positions))
}

fn rank_files(query: &str, nodes: &[String]) -> Vec<FileMatch> {
    if query.trim().is_empty() || nodes.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let mut scored: Vec<FileMatch> = nodes
        .iter()
        .filter_map(|id| {
            fuzzy_file(&needle, id).map(|(score, indices)| FileMatch {
                id: id.clone(),
                score,
                indices,
            })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score));
    scored.truncate(FILE_MATCH_LIMIT);
    scored
}

/// Synthesize a previewable "document" from a NodeMeta — the preview pane
/// renders the metadata as YAML-ish text (egui parity).
fn preview_text_for(meta: &proto::NodeMeta) -> String {
    let mut s = String::new();
    s.push_str(&format!("id:        {}\n", meta.id));
    s.push_str(&format!("title:     {}\n", meta.title));
    s.push_str(&format!("path:      {}\n", meta.path));
    s.push_str(&format!("folder:    {}\n", meta.folder));
    if let Some(dt) = &meta.doctype {
        s.push_str(&format!("doctype:   {}\n", dt));
    }
    if !meta.tags.is_empty() {
        s.push_str(&format!("tags:      [{}]\n", meta.tags.join(", ")));
    }
    s.push_str("\n# metrics\n");
    s.push_str(&format!("degree:    {}\n", meta.degree));
    s.push_str(&format!("indegree:  {}\n", meta.indegree));
    s.push_str(&format!("outdegree: {}\n", meta.outdegree));
    s.push_str(&format!("pagerank:  {:.6}\n", meta.pagerank));
    s.push_str(&format!("kcore:     {}\n", meta.kcore));
    s.push_str(&format!("community: {}\n", meta.community));
    s.push_str(&format!("wcc:       {}\n", meta.wcc));
    if !meta.frontmatter_json.is_empty() && meta.frontmatter_json != "{}" {
        s.push_str("\n# frontmatter\n");
        match serde_json::from_str::<serde_json::Value>(&meta.frontmatter_json) {
            Ok(v) => match serde_json::to_string_pretty(&v) {
                Ok(pp) => s.push_str(&pp),
                Err(_) => s.push_str(&meta.frontmatter_json),
            },
            Err(_) => s.push_str(&meta.frontmatter_json),
        }
    }
    s
}

/// `text` split into spans, matched byte positions wrapped in `hit_class`
/// — the Dioxus form of the egui LayoutJob highlighters. Chunked per char
/// (not per byte) so multi-byte text can't split mid-codepoint.
fn highlight_spans(text: &str, hits: &[usize], hit_class: &'static str) -> Element {
    if hits.is_empty() {
        return rsx! { span { "{text}" } };
    }
    let mut chunks: Vec<(String, bool)> = Vec::new();
    for (i, ch) in text.char_indices() {
        let hit = hits.contains(&i);
        match chunks.last_mut() {
            Some((s, h)) if *h == hit => s.push(ch),
            _ => chunks.push((ch.to_string(), hit)),
        }
    }
    rsx! {
        for (i, (chunk, hit)) in chunks.into_iter().enumerate() {
            span { key: "{i}", class: if hit { hit_class } else { "" }, "{chunk}" }
        }
    }
}

// --- palette state ---------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct PaletteState {
    open: bool,
    query: String,
    /// Stack of parent action ids ("settings" → "filter-actions" etc.).
    breadcrumb: Vec<String>,
    selected_idx: usize,
}

impl PaletteState {
    fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.breadcrumb.clear();
        self.selected_idx = 0;
    }
    fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.breadcrumb.clear();
        self.selected_idx = 0;
    }
    fn toggle(&mut self) {
        if self.open { self.close() } else { self.open() }
    }
}

static PALETTE: GlobalSignal<PaletteState> = Signal::global(PaletteState::default);
static REGISTRY: GlobalSignal<ActionRegistry> = Signal::global(|| {
    let mut reg = ActionRegistry::default();
    seed_default_actions(&mut reg);
    reg
});

/// Successful preview fetches keyed by node id (egui `preview_cache`).
static PREVIEW_CACHE: GlobalSignal<HashMap<String, proto::NodeMeta>> =
    Signal::global(HashMap::new);
/// Failed-fetch ids → error message; avoids re-fetching forever.
static PREVIEW_ERRORS: GlobalSignal<HashMap<String, String>> = Signal::global(HashMap::new);

thread_local! {
    /// Ids with a preview fetch in flight — a thread_local (not a signal)
    /// so the render path can arm fetches without writing reactive state.
    static PREVIEW_INFLIGHT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Read-only snapshot of the recorded action instances.
// Consumer is the Instances panel's actions_section (a stub until it wires
// this in) — same contract as the egui Instances section reading the
// App-owned registry.
#[allow(dead_code)]
pub(crate) fn instances_snapshot() -> Vec<ActionInstance> {
    REGISTRY.read().instances.clone()
}

/// Remove one recorded instance (the Instances panel's per-card ✕).
#[allow(dead_code)]
pub(crate) fn remove_instance(id: u64) {
    REGISTRY.write().instances.retain(|i| i.id != id);
}

/// Human-readable title for an action id (Instances panel card headers).
#[allow(dead_code)]
pub(crate) fn action_title(action_id: &str) -> Option<String> {
    REGISTRY.read().get(action_id).map(|a| a.title.clone())
}

// --- row computation (shared by key handling + rendering) --------------------------

fn ranked_actions(
    reg: &ActionRegistry,
    breadcrumb: &[String],
    query: &str,
) -> Vec<(Action, MatchInfo)> {
    let scope: Vec<&Action> = match breadcrumb.last() {
        Some(parent) => reg.child_actions(parent),
        None => reg.root_actions(),
    };
    if query.trim().is_empty() {
        scope.into_iter().map(|a| (a.clone(), MatchInfo::default())).collect()
    } else {
        let mut scored: Vec<(Action, MatchInfo)> = scope
            .into_iter()
            .filter_map(|a| fuzzy_score(a, query).map(|m| (a.clone(), m)))
            .collect();
        scored.sort_by(|x, y| y.1.score.cmp(&x.1.score));
        scored
    }
}

/// Owned (ranked actions, file matches) snapshot for the event handlers —
/// peeks every signal so nothing subscribes from a handler.
fn rows(ctx: &Ctx) -> (Vec<(Action, MatchInfo)>, Vec<FileMatch>) {
    let (breadcrumb, query) = {
        let p = PALETTE.peek();
        (p.breadcrumb.clone(), p.query.clone())
    };
    let ranked = {
        let reg = REGISTRY.peek();
        ranked_actions(&reg, &breadcrumb, &query)
    };
    // File matches only on the root scope (drilled-in scopes are
    // action-only), same as the egui palette.
    let files = if breadcrumb.is_empty() {
        let g = ctx.graph.peek();
        let ids: &[String] = g.as_ref().map(|x| x.ids.as_slice()).unwrap_or(&[]);
        rank_files(&query, ids)
    } else {
        Vec::new()
    };
    (ranked, files)
}

// --- key handling ------------------------------------------------------------------

/// Global key hook, called from the app root's `onkeydown` BEFORE
/// `panel_kit::is_editing()` and the camera keys (so the open chord works
/// while an input has focus). Returns `true` iff the event was consumed:
/// the Ctrl/⌘+P chord, and every key while the palette is open.
pub(crate) fn handle_key(e: &KeyboardEvent, ctx: Ctx) -> bool {
    // Ctrl+P / ⌘+P toggle — the same coalesced chord as the egui binding
    // (`Mods::command()` covers macOS ⌘ and Linux/Windows Ctrl).
    let mods = e.modifiers();
    let command = mods.contains(Modifiers::CONTROL) || mods.contains(Modifiers::META);
    if command && !mods.contains(Modifiers::ALT) && !mods.contains(Modifiers::SHIFT) {
        if let Key::Character(c) = e.key() {
            if c.eq_ignore_ascii_case("p") {
                e.prevent_default(); // beat the browser's print dialog
                PALETTE.write().toggle();
                return true;
            }
        }
    }
    if !PALETTE.peek().open {
        return false;
    }

    // Palette is open: every key is consumed (the camera never sees it).
    // Plain typing keys fall through to the input's own default handling.
    let configuring = REGISTRY.peek().configuring.is_some();
    match e.key() {
        // Esc closes — or cancels the parameter form if one is active.
        Key::Escape => {
            if configuring {
                REGISTRY.write().cancel_configuring();
            } else {
                PALETTE.write().close();
            }
        }
        Key::Enter => {
            e.prevent_default();
            if configuring {
                form_apply_or_next();
            } else {
                activate_selected(ctx);
            }
        }
        Key::ArrowDown if !configuring => {
            e.prevent_default();
            move_selection(ctx, 1);
        }
        Key::ArrowUp if !configuring => {
            e.prevent_default();
            move_selection(ctx, -1);
        }
        // Tab completes the query with the selected title / file id.
        Key::Tab if !configuring => {
            e.prevent_default();
            tab_complete(ctx);
        }
        // Backspace on an empty query pops the breadcrumb.
        Key::Backspace if !configuring => {
            let pop = {
                let p = PALETTE.peek();
                p.query.is_empty() && !p.breadcrumb.is_empty()
            };
            if pop {
                let mut p = PALETTE.write();
                p.breadcrumb.pop();
                p.selected_idx = 0;
            }
        }
        _ => {}
    }
    true
}

fn move_selection(ctx: Ctx, dir: i32) {
    let (ranked, files) = rows(&ctx);
    let total = ranked.len() + files.len();
    if total == 0 {
        return;
    }
    let mut p = PALETTE.write();
    let cur = if p.selected_idx >= total { 0 } else { p.selected_idx };
    p.selected_idx = if dir > 0 { (cur + 1) % total } else { (cur + total - 1) % total };
}

fn tab_complete(ctx: Ctx) {
    let (ranked, files) = rows(&ctx);
    let total = ranked.len() + files.len();
    if total == 0 {
        return;
    }
    let sel = {
        let i = PALETTE.peek().selected_idx;
        if i >= total { 0 } else { i }
    };
    let completion = if sel < ranked.len() {
        ranked[sel].0.title.clone()
    } else {
        files[sel - ranked.len()].id.clone()
    };
    PALETTE.write().query = completion;
}

fn activate_selected(ctx: Ctx) {
    let (ranked, files) = rows(&ctx);
    let total = ranked.len() + files.len();
    if total == 0 {
        return;
    }
    let sel = {
        let i = PALETTE.peek().selected_idx;
        if i >= total { 0 } else { i }
    };
    if sel < ranked.len() {
        enter_action(&ranked[sel].0);
    } else if let Some(fm) = files.get(sel - ranked.len()) {
        open_node(ctx, fm.id.clone());
    }
}

/// Drill into children / open the parameter form / execute immediately —
/// port of the egui `enter_action`.
fn enter_action(action: &Action) {
    if parity_gap(&action.id).is_some() {
        return; // PARITY GAP rows are inert (see `parity_gap`)
    }
    if !action.children_ids.is_empty() {
        let mut p = PALETTE.write();
        p.breadcrumb.push(action.id.clone());
        p.query.clear();
        p.selected_idx = 0;
        return;
    }
    if action.parameters.is_empty() {
        PALETTE.write().close();
        execute_action(&action.id, HashMap::new());
        return;
    }
    // Parameterized: enter form mode, smart-defaulting the Settings
    // sub-tree from the live workspace settings.
    let initial = if action.parent_id.as_deref() == Some("settings") {
        workspace_initial_for(action)
    } else {
        HashMap::new()
    };
    REGISTRY.write().start_configuring(&action.id, &initial);
}

/// User chose a fuzzy-matched vault file — route it through the selection
/// signal (the Dioxus equivalent of the egui host's node-detail fetch).
fn open_node(ctx: Ctx, id: String) {
    PALETTE.write().close();
    let mut selected = ctx.selected;
    selected.set(Some(id));
}

/// Enter inside the parameter form: advances on earlier params, applies on
/// the last — both gated on the current param validating.
fn form_apply_or_next() {
    let snapshot = {
        let reg = REGISTRY.peek();
        reg.configuring.as_ref().and_then(|cfg| {
            reg.get(&cfg.action_id)
                .map(|a| (a.clone(), cfg.current_param_index, cfg.form_values.clone()))
        })
    };
    let Some((action, raw_idx, values)) = snapshot else { return };
    let n = action.parameters.len();
    let idx = raw_idx.min(n.saturating_sub(1));
    let Some(param) = action.parameters.get(idx) else { return };
    if check_param(param, values.get(&param.id)).is_err() {
        return;
    }
    if idx + 1 >= n {
        if let Some((id, params)) = REGISTRY.write().take_finished_form() {
            execute_action(&id, params);
        }
    } else if let Some(c) = REGISTRY.write().configuring.as_mut() {
        c.current_param_index = idx + 1;
    }
}

// --- execution dispatch (port of app.rs::execute_action / run_builtin) -------------

fn execute_action(action_id: &str, params: HashMap<String, ParamValue>) {
    let Some(action) = REGISTRY.peek().get(action_id).cloned() else { return };
    // Parent-only actions drill into children; they should not produce
    // instances even if accidentally executed.
    if !action.children_ids.is_empty() && action.parameters.is_empty() {
        return;
    }
    // Attribute the snapshot/event to the palette action by its title —
    // the egui `snapshot_source` + frontend-event pair.
    crate::appstate::note_mutation("palette", &action.title);
    let ActionHandler::Builtin(variant) = action.handler;
    let result = run_builtin(variant, &params);
    REGISTRY.write().record_execution(&action.id, params, result);
}

/// Append a Filter card to the shared query model, prefixed with an AND
/// connector when needed — port of `app.rs::append_filter_card`.
///
/// PARITY GAP: the egui version also opened the Filter sidebar section so
/// the user sees the addition land; panel open state lives in main.rs's
/// `use_workspace` and is not reachable from here.
fn append_filter_card(field: String, value: String) {
    filter::edit_filters(|q| {
        let needs_connector = !matches!(
            q.cards.last(),
            None | Some(Card::Connector { .. }) | Some(Card::ParenOpen) | Some(Card::Not)
        );
        if needs_connector {
            q.cards.push(Card::Connector { op: ConnectorOp::And });
        }
        q.cards.push(Card::Filter { field, op: Op::Eq, value });
    });
}

fn run_builtin(
    variant: BuiltinAction,
    params: &HashMap<String, ParamValue>,
) -> serde_json::Value {
    use BuiltinAction as B;
    match variant {
        B::Settings | B::NodeOperations | B::Filter => serde_json::json!({}),

        B::EditOptions => {
            update_workspace(|ws| {
                if let Some(ParamValue::Number(n)) = params.get("font_size") {
                    ws.font_size = (*n as f32).clamp(8.0, 32.0);
                }
                if let Some(ParamValue::Selected(items)) = params.get("font_family") {
                    if let Some(v) = items.first() {
                        ws.font_family = parse_font_family(v);
                    }
                }
                if let Some(ParamValue::Boolean(b)) = params.get("show_line_numbers") {
                    ws.show_line_numbers = *b;
                }
            });
            let ws = *WORKSPACE.peek();
            serde_json::json!({ "settings": {
                "font_size": ws.font_size,
                "font_family": format!("{:?}", ws.font_family),
                "show_line_numbers": ws.show_line_numbers,
            } })
        }
        B::FontSize => {
            update_workspace(|ws| {
                if let Some(ParamValue::Number(n)) = params.get("font_size") {
                    ws.font_size = (*n as f32).clamp(8.0, 32.0);
                }
            });
            serde_json::json!({ "font_size": WORKSPACE.peek().font_size })
        }
        B::FontFamily => {
            update_workspace(|ws| {
                if let Some(ParamValue::Selected(items)) = params.get("font_family") {
                    if let Some(v) = items.first() {
                        ws.font_family = parse_font_family(v);
                    }
                }
            });
            serde_json::json!({
                "font_family": format!("{:?}", WORKSPACE.peek().font_family)
            })
        }
        B::LineNumbers => {
            update_workspace(|ws| {
                if let Some(ParamValue::Boolean(b)) = params.get("show_line_numbers") {
                    ws.show_line_numbers = *b;
                }
            });
            serde_json::json!({ "show_line_numbers": WORKSPACE.peek().show_line_numbers })
        }
        B::ToggleTheme => {
            // Dark-mode only; records intent without flipping anything —
            // exact egui parity.
            serde_json::json!({ "theme": "dark" })
        }

        B::FilterByName | B::FilterByContent => {
            let field = if matches!(variant, B::FilterByName) { "name" } else { "content" };
            let pattern = params
                .get("pattern")
                .and_then(|v| v.as_string())
                .unwrap_or("")
                .to_string();
            append_filter_card(field.to_string(), pattern.clone());
            serde_json::json!({ "filter": { "type": field, "pattern": pattern } })
        }
        B::FilterByTag => {
            let tags: Vec<String> = params
                .get("tags")
                .and_then(|v| v.as_selected())
                .unwrap_or(&[])
                .to_vec();
            for t in &tags {
                append_filter_card("tag".into(), t.clone());
            }
            serde_json::json!({ "filter": { "type": "tag", "tags": tags } })
        }
        B::SearchNodes => {
            let q = params
                .get("query")
                .and_then(|v| v.as_string())
                .unwrap_or("")
                .to_string();
            if !q.is_empty() {
                let q2 = q.clone();
                filter::edit_filters(|m| {
                    m.cards.push(Card::Search { value: q2, regex: false });
                });
            }
            serde_json::json!({ "search": { "query": q } })
        }
        B::CreateNode => {
            // Node creation against a server-loaded vault is a no-op —
            // the server owns the vault. Recorded for parity.
            let name = params
                .get("name")
                .and_then(|v| v.as_string())
                .unwrap_or("New Node")
                .to_string();
            serde_json::json!({ "node": { "name": name } })
        }

        B::FitCamera => {
            render::fit_camera();
            serde_json::json!({ "fit": true })
        }

        // egui: `state.style = Default::default()`.
        B::ResetStyle => {
            crate::panels::style::reset_to_defaults();
            serde_json::json!({ "style": "defaults" })
        }
        // egui: `state.set_section_open(sec, true)` — here a restore+raise
        // request the App root drains into `Workspace::restore`.
        B::JumpToSection(sec) => {
            *crate::OPEN_PANEL.write() = Some(sec.panel());
            serde_json::json!({ "open": sec.title() })
        }
        // Unreachable: `enter_action` refuses parity-gap actions before
        // dispatch; kept for match exhaustivity, greppable next to its gap.
        // PARITY GAP: egui did `state.dock.push_tab(TabKind::Graph)`.
        B::NewGraphTab => serde_json::json!({ "parity_gap": "new-graph-tab" }),
    }
}

// --- preview fetch -----------------------------------------------------------------

/// Arm a `/node/:id` fetch for the selected file row's preview, once per
/// id — the Dioxus form of the egui `pending_preview_id` → host-fetch loop.
fn request_preview(id: &str) {
    if PREVIEW_CACHE.peek().contains_key(id) || PREVIEW_ERRORS.peek().contains_key(id) {
        return;
    }
    if PREVIEW_INFLIGHT.with(|s| !s.borrow_mut().insert(id.to_string())) {
        return;
    }
    let id = id.to_string();
    spawn(async move {
        match api::node_meta(&id).await {
            Ok(m) => {
                PREVIEW_CACHE.write().insert(id.clone(), m);
            }
            Err(e) => {
                PREVIEW_ERRORS.write().insert(id.clone(), e);
            }
        }
        PREVIEW_INFLIGHT.with(|s| {
            s.borrow_mut().remove(&id);
        });
    });
}

// --- overlay rendering ---------------------------------------------------------------

/// Palette overlay, rendered unconditionally at the app root (empty when
/// closed). Owns its own open/query/selection state.
pub(crate) fn overlay(ctx: Ctx) -> Element {
    let st = PALETTE.read().clone();
    if !st.open {
        return rsx! {};
    }
    let configuring = REGISTRY.read().configuring.is_some();
    // Wider window when the file-preview pane may be in play (egui clamps
    // to 90% of the viewport; max-width in CSS does the same here).
    let wide = {
        let g = ctx.graph.read();
        let has_nodes = g.as_ref().map(|x| !x.ids.is_empty()).unwrap_or(false);
        !configuring && !st.query.trim().is_empty() && has_nodes
    };
    let body = if configuring { render_param_form() } else { render_search(ctx, &st) };
    rsx! {
        div { class: "cp-overlay",
            div { class: if wide { "cp-panel wide" } else { "cp-panel" }, {body} }
        }
    }
}

// ----- search / list mode --------------------------------------------------

fn render_search(ctx: Ctx, st: &PaletteState) -> Element {
    let (crumbs, ranked) = {
        let reg = REGISTRY.read();
        let crumbs: Vec<String> = st
            .breadcrumb
            .iter()
            .map(|pid| reg.get(pid).map(|a| a.title.clone()).unwrap_or_else(|| pid.clone()))
            .collect();
        (crumbs, ranked_actions(&reg, &st.breadcrumb, &st.query))
    };
    let files = if st.breadcrumb.is_empty() {
        let g = ctx.graph.read();
        let ids: &[String] = g.as_ref().map(|x| x.ids.as_slice()).unwrap_or(&[]);
        rank_files(&st.query, ids)
    } else {
        Vec::new()
    };

    let total = ranked.len() + files.len();
    let sel = if total == 0 || st.selected_idx >= total { 0 } else { st.selected_idx };
    // Group root entries by category when the user hasn't typed anything
    // and isn't drilled into a child scope.
    let group_by_category = st.breadcrumb.is_empty() && st.query.trim().is_empty();
    let two_pane = !files.is_empty();

    // Resolve the selected file id (if any) and arm its preview fetch.
    let selected_file: Option<String> = if sel >= ranked.len() {
        files.get(sel - ranked.len()).map(|f| f.id.clone())
    } else {
        None
    };
    if let Some(id) = &selected_file {
        request_preview(id);
    }

    // Flat row list in render order — `sel` indexes this same order
    // (categories preserve registration order, so grouped == flat).
    let mut items: Vec<Element> = Vec::new();
    let mut row_idx = 0usize;
    if total == 0 {
        items.push(rsx! { div { class: "cp-empty", "No matching actions or files" } });
    } else {
        if group_by_category {
            // Stable category order (first-seen); uncategorised last.
            let mut by_cat: Vec<(String, Vec<(Action, MatchInfo)>)> = Vec::new();
            let mut uncategorised: Vec<(Action, MatchInfo)> = Vec::new();
            for (a, mi) in &ranked {
                match &a.category {
                    Some(c) => match by_cat.iter_mut().find(|(name, _)| name == c) {
                        Some(slot) => slot.1.push((a.clone(), mi.clone())),
                        None => by_cat.push((c.clone(), vec![(a.clone(), mi.clone())])),
                    },
                    None => uncategorised.push((a.clone(), mi.clone())),
                }
            }
            for (cat, group) in by_cat {
                items.push(rsx! { div { class: "cp-cat", "{cat}" } });
                for (a, mi) in group {
                    items.push(action_row(&a, &mi, row_idx, sel));
                    row_idx += 1;
                }
            }
            if !uncategorised.is_empty() {
                items.push(rsx! { div { class: "cp-cat", "Other" } });
                for (a, mi) in uncategorised {
                    items.push(action_row(&a, &mi, row_idx, sel));
                    row_idx += 1;
                }
            }
        } else {
            for (a, mi) in &ranked {
                items.push(action_row(a, mi, row_idx, sel));
                row_idx += 1;
            }
        }
        // File matches (fzf-style: actions first, then files).
        if !files.is_empty() {
            if !ranked.is_empty() {
                items.push(rsx! { div { class: "cp-sep" } });
            }
            items.push(rsx! { div { class: "cp-cat", "Files / Nodes" } });
            for fm in &files {
                items.push(file_row(ctx, fm, row_idx, sel));
                row_idx += 1;
            }
        }
    }

    let query = st.query.clone();
    let n_crumbs = crumbs.len();
    rsx! {
        if n_crumbs > 0 {
            div { class: "cp-crumbs",
                for (i, title) in crumbs.into_iter().enumerate() {
                    button {
                        key: "{i}",
                        class: "cp-crumb",
                        // Keep focus in the search input across clicks.
                        onmousedown: move |e| e.prevent_default(),
                        onclick: move |_| {
                            let mut p = PALETTE.write();
                            p.breadcrumb.truncate(i + 1);
                            p.query.clear();
                            p.selected_idx = 0;
                        },
                        "{title}"
                    }
                }
            }
        }
        input {
            class: "cp-input",
            value: "{query}",
            placeholder: "Type a command or fuzzy-search vault files…",
            onmounted: move |e: MountedEvent| {
                spawn(async move {
                    let _ = e.data().set_focus(true).await;
                });
            },
            oninput: move |e| {
                PALETTE.write().query = e.value();
            },
        }
        div { class: "cp-body",
            div { class: "cp-list", {items.into_iter()} }
            if two_pane {
                div { class: "cp-preview", {render_preview(&selected_file)} }
            }
        }
    }
}

fn action_row(action: &Action, mi: &MatchInfo, row_idx: usize, sel: usize) -> Element {
    let active = row_idx == sel;
    let gap = parity_gap(&action.id);
    let class = match (active, gap.is_some()) {
        (true, true) => "cp-row active disabled",
        (true, false) => "cp-row active",
        (false, true) => "cp-row disabled",
        (false, false) => "cp-row",
    };
    let title_el = highlight_spans(&action.title, &mi.title_hits, "cp-hit");
    let desc = action.description.clone();
    let kind = action.kind.label();
    let shortcut = action.shortcut.clone();
    let has_children = !action.children_ids.is_empty();
    let a2 = action.clone();
    rsx! {
        div {
            class: "{class}",
            title: gap.unwrap_or_default(),
            onmousedown: move |e| e.prevent_default(),
            onmouseenter: move |_| {
                if PALETTE.peek().selected_idx != row_idx {
                    PALETTE.write().selected_idx = row_idx;
                }
            },
            onclick: move |_| enter_action(&a2),
            div { class: "cp-main",
                div { class: "cp-title", {title_el} }
                if !desc.is_empty() {
                    div { class: "cp-desc", "{desc}" }
                }
            }
            div { class: "cp-side",
                if let Some(sc) = shortcut {
                    span { class: "cp-shortcut", "{sc}" }
                }
                span { class: "cp-kind", "{kind}" }
                if has_children {
                    span { class: "cp-child", "›" }
                }
            }
        }
    }
}

fn file_row(ctx: Ctx, fm: &FileMatch, row_idx: usize, sel: usize) -> Element {
    let active = row_idx == sel;
    // The filename is the primary label; the folder is the secondary line.
    let folder = match fm.id.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    };
    let path_el = highlight_spans(&fm.id, &fm.indices, "cp-path-hit");
    let id2 = fm.id.clone();
    rsx! {
        div {
            class: if active { "cp-row file active" } else { "cp-row file" },
            onmousedown: move |e| e.prevent_default(),
            onmouseenter: move |_| {
                if PALETTE.peek().selected_idx != row_idx {
                    PALETTE.write().selected_idx = row_idx;
                }
            },
            onclick: move |_| open_node(ctx, id2.clone()),
            div { class: "cp-main",
                div { class: "cp-path", {path_el} }
                if !folder.is_empty() {
                    div { class: "cp-folder", "{folder}" }
                }
            }
        }
    }
}

fn render_preview(selected: &Option<String>) -> Element {
    let Some(id) = selected else {
        return rsx! { div { class: "cp-preview-hint", "(no file selected)" } };
    };
    if let Some(err) = PREVIEW_ERRORS.read().get(id) {
        return rsx! { div { class: "cp-preview-err", "preview error: {err}" } };
    }
    let cache = PREVIEW_CACHE.read();
    let Some(meta) = cache.get(id) else {
        return rsx! { div { class: "cp-preview-hint", "loading…" } };
    };
    let body = preview_text_for(meta);
    rsx! { pre { class: "cp-preview-doc", "{body}" } }
}

// ----- parameter form mode -------------------------------------------------

fn render_param_form() -> Element {
    let (action, raw_idx, values) = {
        let reg = REGISTRY.read();
        let Some(cfg) = reg.configuring.as_ref() else { return rsx! {} };
        let Some(action) = reg.get(&cfg.action_id).cloned() else { return rsx! {} };
        (action, cfg.current_param_index, cfg.form_values.clone())
    };
    let n_params = action.parameters.len();
    let idx = raw_idx.min(n_params.saturating_sub(1));
    let is_last = idx + 1 >= n_params;
    let Some(param) = action.parameters.get(idx).cloned() else { return rsx! {} };
    let value = values
        .get(&param.id)
        .cloned()
        .unwrap_or_else(|| ParamValue::default_for(param.kind));
    // Live validity drives the disabled state of Next/Apply and the error
    // label (the egui app re-validated every frame).
    let err = check_param(&param, Some(&value)).err();
    let valid = err.is_none();
    let label = if param.required {
        format!("{} *", param.name)
    } else {
        param.name.clone()
    };

    rsx! {
        div { class: "cp-form",
            div { class: "cp-form-title", "Configure {action.title}" }
            div { class: "cp-desc", "{action.description}" }
            if n_params > 1 {
                div { class: "cp-desc", { format!("Parameter {} of {}", idx + 1, n_params) } }
            }
            div { class: "cp-sep" }
            div { class: "cp-form-label", "{label}" }
            div { class: "cp-desc", "{param.description}" }
            {param_widget(&param, &value)}
            if let Some(e) = err {
                div { class: "cp-err", "{e}" }
            }
            // Visual order matches the egui right_to_left row read
            // left→right: Cancel | Previous | Next — or Cancel | Apply on
            // the last parameter (no Previous there; egui quirk kept).
            div { class: "cp-form-btns",
                button { class: "btn",
                    onclick: move |_| REGISTRY.write().cancel_configuring(),
                    "Cancel"
                }
                if !is_last {
                    button { class: "btn", disabled: idx == 0,
                        onclick: move |_| {
                            if let Some(c) = REGISTRY.write().configuring.as_mut() {
                                c.current_param_index = c.current_param_index.saturating_sub(1);
                            }
                        },
                        "Previous"
                    }
                    button { class: "btn", disabled: !valid,
                        onclick: move |_| form_apply_or_next(),
                        "Next"
                    }
                } else {
                    button { class: "btn", disabled: !valid,
                        onclick: move |_| form_apply_or_next(),
                        "Apply"
                    }
                }
            }
        }
    }
}

fn set_form_value(id: &str, v: ParamValue) {
    if let Some(c) = REGISTRY.write().configuring.as_mut() {
        c.form_values.insert(id.to_string(), v);
    }
}

fn toggle_multi(id: &str, value: &str, on: bool) {
    if let Some(c) = REGISTRY.write().configuring.as_mut() {
        if let Some(ParamValue::Selected(items)) = c.form_values.get_mut(id) {
            if on {
                if !items.iter().any(|v| v == value) {
                    items.push(value.to_string());
                }
            } else {
                items.retain(|v| v != value);
            }
        }
    }
}

fn param_widget(param: &ActionParameter, value: &ParamValue) -> Element {
    let pid = param.id.clone();
    match param.kind {
        ParameterType::String => {
            let s = value.as_string().unwrap_or("").to_string();
            rsx! {
                input { class: "cp-form-in", value: "{s}",
                    oninput: move |e| set_form_value(&pid, ParamValue::String(e.value())),
                }
            }
        }
        ParameterType::Number => {
            let n = if let ParamValue::Number(n) = value { *n } else { 0.0 };
            let min = param.validation.min.map(|v| v.to_string()).unwrap_or_default();
            let max = param.validation.max.map(|v| v.to_string()).unwrap_or_default();
            rsx! {
                input { class: "cp-form-in num", r#type: "number", step: "0.1",
                    min: "{min}", max: "{max}", value: "{n}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<f64>() {
                            set_form_value(&pid, ParamValue::Number(v));
                        }
                    },
                }
            }
        }
        ParameterType::Boolean => {
            let b = matches!(value, ParamValue::Boolean(true));
            rsx! {
                label { class: "cp-form-check",
                    input { r#type: "checkbox", checked: b,
                        onchange: move |e| set_form_value(&pid, ParamValue::Boolean(e.checked())),
                    }
                    "Enable"
                }
            }
        }
        ParameterType::Select => {
            let current = value
                .as_selected()
                .and_then(|v| v.first().cloned())
                .unwrap_or_default();
            let opts = param.options.clone();
            rsx! {
                select { class: "cp-form-in",
                    onchange: move |e| set_form_value(&pid, ParamValue::Selected(vec![e.value()])),
                    for o in opts {
                        option { key: "{o.value}", value: "{o.value}",
                            selected: o.value == current,
                            "{o.label}"
                        }
                    }
                }
            }
        }
        ParameterType::MultiSelect => {
            if param.options.is_empty() {
                return rsx! {
                    div { class: "cp-desc", em { "(no options available)" } }
                };
            }
            let selected: Vec<String> =
                value.as_selected().map(<[String]>::to_vec).unwrap_or_default();
            let opts = param.options.clone();
            rsx! {
                div { class: "cp-form-multi",
                    for o in opts {
                        {
                            let on = selected.iter().any(|v| v == &o.value);
                            let pid2 = pid.clone();
                            let val = o.value.clone();
                            rsx! {
                                label { key: "{o.value}", class: "cp-form-check",
                                    input { r#type: "checkbox", checked: on,
                                        onchange: move |e| toggle_multi(&pid2, &val, e.checked()),
                                    }
                                    "{o.label}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
