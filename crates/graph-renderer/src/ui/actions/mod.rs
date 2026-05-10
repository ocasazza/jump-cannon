//! Action registry — typed parameters, singleton vs. multi-instance,
//! hierarchical parent/child, ActionInstance tracking. Ported from the
//! Nuxt frontend's `stores/actions.ts`.
//!
//! Execution dispatch is enum-based via `BuiltinAction`. The `App` owns
//! the registry and runs the dispatch in `app.rs::execute_action` so the
//! handler has direct mutable access to AppState + GraphPipelines.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::state::Section;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionType {
    Singleton,
    MultiInstance,
}

impl ActionType {
    pub fn label(self) -> &'static str {
        match self {
            ActionType::Singleton => "Singleton",
            ActionType::MultiInstance => "Multi-instance",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParameterType {
    String,
    Number,
    Boolean,
    Select,
    MultiSelect,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParameterValidation {
    pub pattern: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterOption {
    pub value: String,
    pub label: String,
}

/// Type-safe parameter value. Mirrors the runtime form values in
/// `CommandPalette.vue` but keeps the form state machine strongly typed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ParamValue {
    String(String),
    Number(f64),
    Boolean(bool),
    /// Used for both Select (single value) and MultiSelect (n values).
    /// Select stores a 1-element vec for a uniform widget contract.
    Selected(Vec<String>),
}

impl ParamValue {
    pub fn default_for(ty: ParameterType) -> Self {
        match ty {
            ParameterType::String => ParamValue::String(String::new()),
            ParameterType::Number => ParamValue::Number(0.0),
            ParameterType::Boolean => ParamValue::Boolean(false),
            ParameterType::Select => ParamValue::Selected(Vec::new()),
            ParameterType::MultiSelect => ParamValue::Selected(Vec::new()),
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        if let ParamValue::String(s) = self { Some(s) } else { None }
    }
    pub fn as_number(&self) -> Option<f64> {
        if let ParamValue::Number(n) = self { Some(*n) } else { None }
    }
    pub fn as_bool(&self) -> Option<bool> {
        if let ParamValue::Boolean(b) = self { Some(*b) } else { None }
    }
    pub fn as_selected(&self) -> Option<&[String]> {
        if let ParamValue::Selected(v) = self { Some(v.as_slice()) } else { None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionParameter {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ParameterType,
    pub required: bool,
    pub default: Option<ParamValue>,
    #[serde(default)]
    pub options: Vec<ParameterOption>,
    #[serde(default)]
    pub validation: ParameterValidation,
}

/// Built-in action handlers. The dispatch lives in `app.rs::execute_action`
/// where it has access to AppState + GraphPipelines. Carrying the variant
/// in `ActionHandler` keeps actions inspectable (vs. boxed closures) and
/// removes the need to thread per-handler trait objects through state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BuiltinAction {
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
    /// Open a new Graph tab in the central workspace dock.
    NewGraphTab,
    /// Toggle the right-hand inspector sidebar open/collapsed.
    ToggleInspector,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionHandler {
    Builtin(BuiltinAction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub kind: ActionType,
    #[serde(default)]
    pub parameters: Vec<ActionParameter>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub children_ids: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub contextual: bool,
    /// Optional human-readable keyboard shortcut hint shown next to the
    /// title in the palette (e.g. "Ctrl+P", "F5"). Display-only — actual
    /// shortcut binding lives in the input handler.
    #[serde(default)]
    pub shortcut: Option<String>,
    pub handler: ActionHandler,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionInstance {
    pub id: u64,
    pub action_id: String,
    /// Free-form output from the handler. JSON for heterogeneity across
    /// handlers; rendered read-only in the Instances panel.
    pub state: serde_json::Value,
    #[serde(default)]
    pub params: HashMap<String, ParamValue>,
}

/// In-progress parameter form. `None` when not configuring.
#[derive(Debug, Clone, Default)]
pub struct ConfiguringState {
    pub action_id: String,
    pub current_param_index: usize,
    pub form_values: HashMap<String, ParamValue>,
    pub validation_errors: HashMap<String, String>,
    /// MultiSelect editing is mirrored into form_values on each frame —
    /// this dual map lets the checkbox set/unset toggle write back into
    /// `form_values[param_id]: ParamValue::Selected(_)`. Kept for parity
    /// with the Vue version's `multiSelectValues` pattern.
    pub multi_select_values: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default)]
pub struct ActionRegistry {
    pub actions: Vec<Action>,
    pub instances: Vec<ActionInstance>,
    pub configuring: Option<ConfiguringState>,
    pub next_instance_id: u64,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, action: Action) {
        if let Some(existing) = self.actions.iter_mut().find(|a| a.id == action.id) {
            *existing = action;
        } else {
            self.actions.push(action);
        }
    }

    pub fn unregister(&mut self, id: &str) {
        self.actions.retain(|a| a.id != id);
        self.instances.retain(|i| i.action_id != id);
    }

    pub fn get(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn root_actions(&self) -> Vec<&Action> {
        self.actions.iter().filter(|a| a.parent_id.is_none()).collect()
    }

    pub fn child_actions(&self, parent_id: &str) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| a.parent_id.as_deref() == Some(parent_id))
            .collect()
    }

    pub fn categories(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for a in &self.actions {
            if let Some(c) = &a.category {
                if !seen.iter().any(|s| s == c) {
                    seen.push(c.clone());
                }
            }
        }
        seen
    }

    pub fn actions_in_category(&self, category: &str) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| a.category.as_deref() == Some(category))
            .collect()
    }

    /// Begin a parameter form for `action_id`. Pre-fills `form_values`
    /// from `initial_values` (smart defaults) when present; otherwise
    /// each parameter's `default` (or its type default).
    pub fn start_configuring(
        &mut self,
        action_id: &str,
        initial_values: &HashMap<String, ParamValue>,
    ) {
        let Some(action) = self.get(action_id).cloned() else { return };
        let mut form_values: HashMap<String, ParamValue> = HashMap::new();
        let mut multi_select_values: HashMap<String, Vec<String>> = HashMap::new();
        for p in &action.parameters {
            let v = initial_values
                .get(&p.id)
                .cloned()
                .or_else(|| p.default.clone())
                .unwrap_or_else(|| ParamValue::default_for(p.kind));
            if matches!(p.kind, ParameterType::MultiSelect) {
                if let ParamValue::Selected(items) = &v {
                    multi_select_values.insert(p.id.clone(), items.clone());
                }
            }
            form_values.insert(p.id.clone(), v);
        }
        self.configuring = Some(ConfiguringState {
            action_id: action_id.to_string(),
            current_param_index: 0,
            form_values,
            validation_errors: HashMap::new(),
            multi_select_values,
        });
    }

    pub fn cancel_configuring(&mut self) {
        self.configuring = None;
    }

    /// Validate the parameter at `index` of the configuring action.
    /// Sets/clears `validation_errors[param.id]`. Returns true if valid.
    pub fn validate_param(&mut self, index: usize) -> bool {
        let Some(cfg) = self.configuring.as_mut() else { return true };
        let Some(action) = self.actions.iter().find(|a| a.id == cfg.action_id) else {
            return true;
        };
        let Some(param) = action.parameters.get(index) else { return true };
        let value = cfg.form_values.get(&param.id);

        // Required check.
        if param.required {
            let empty = match value {
                None => true,
                Some(ParamValue::String(s)) => s.is_empty(),
                Some(ParamValue::Selected(v)) => v.is_empty(),
                _ => false,
            };
            if empty {
                cfg.validation_errors.insert(
                    param.id.clone(),
                    "This field is required".to_string(),
                );
                return false;
            }
        }

        // Type-specific validation.
        match (param.kind, value) {
            (ParameterType::Number, Some(ParamValue::Number(n))) => {
                if let Some(min) = param.validation.min {
                    if *n < min {
                        cfg.validation_errors.insert(
                            param.id.clone(),
                            format!("Minimum value is {min}"),
                        );
                        return false;
                    }
                }
                if let Some(max) = param.validation.max {
                    if *n > max {
                        cfg.validation_errors.insert(
                            param.id.clone(),
                            format!("Maximum value is {max}"),
                        );
                        return false;
                    }
                }
            }
            (ParameterType::String, Some(ParamValue::String(s))) => {
                if let Some(pat) = &param.validation.pattern {
                    // Cheap substring check (full regex not pulled in for
                    // a single use). Pattern is a hint, not a strict gate.
                    if !pat.is_empty() && !s.contains(pat) {
                        cfg.validation_errors.insert(
                            param.id.clone(),
                            format!("Should contain `{pat}`"),
                        );
                        return false;
                    }
                }
            }
            _ => {}
        }

        cfg.validation_errors.remove(&param.id);
        true
    }

    /// Finalize the form — caller should call `App::execute_action` with
    /// the returned (action_id, params). Clears `configuring`.
    pub fn take_finished_form(&mut self) -> Option<(String, HashMap<String, ParamValue>)> {
        let cfg = self.configuring.take()?;
        Some((cfg.action_id, cfg.form_values))
    }

    /// Record an ActionInstance (or update the existing one for singletons).
    /// Returns the instance id.
    pub fn record_execution(
        &mut self,
        action_id: &str,
        params: HashMap<String, ParamValue>,
        state: serde_json::Value,
    ) -> u64 {
        let kind = self.get(action_id).map(|a| a.kind).unwrap_or(ActionType::MultiInstance);
        if kind == ActionType::Singleton {
            if let Some(existing) = self.instances.iter_mut().find(|i| i.action_id == action_id) {
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

    pub fn remove_instance(&mut self, id: u64) {
        self.instances.retain(|i| i.id != id);
    }
}

mod builtins;
pub use builtins::seed_default_actions;
