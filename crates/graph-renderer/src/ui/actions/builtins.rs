//! Default action set — port of `archive/nuxt/plugins/register-actions.ts`.
//!
//! Lives in its own module so the registry types in `actions/mod.rs`
//! stay focused on the data model + dispatch surface, while this file
//! is purely declarative seed data.

use super::{
    Action, ActionHandler, ActionParameter, ActionRegistry, ActionType, BuiltinAction,
    ParamValue, ParameterOption, ParameterType, ParameterValidation,
};
use crate::ui::state::Section;

pub fn seed_default_actions(reg: &mut ActionRegistry) {
    use BuiltinAction::*;

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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(Settings),
    });

    reg.register(Action {
        id: "edit-options".into(),
        title: "Edit Options".into(),
        description: "Configure application settings".into(),
        keywords: words(&["settings", "options", "preferences", "configure"]),
        kind: ActionType::Singleton,
        parameters: vec![
            ActionParameter {
                id: "font_size".into(),
                name: "Font Size".into(),
                description: "Font size in pixels".into(),
                kind: ParameterType::Number,
                required: true,
                default: Some(ParamValue::Number(14.0)),
                options: vec![],
                validation: ParameterValidation {
                    pattern: None,
                    min: Some(8.0),
                    max: Some(32.0),
                },
            },
            ActionParameter {
                id: "font_family".into(),
                name: "Font Family".into(),
                description: "Font family for the editor".into(),
                kind: ParameterType::Select,
                required: true,
                default: Some(ParamValue::Selected(vec!["monospace".into()])),
                options: font_family_options(),
                validation: ParameterValidation::default(),
            },
            ActionParameter {
                id: "show_line_numbers".into(),
                name: "Show Line Numbers".into(),
                description: "Display line numbers in the editor".into(),
                kind: ParameterType::Boolean,
                required: false,
                default: Some(ParamValue::Boolean(true)),
                options: vec![],
                validation: ParameterValidation::default(),
            },
        ],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(EditOptions),
    });

    reg.register(Action {
        id: "font-size".into(),
        title: "Change Font Size".into(),
        description: "Adjust the font size".into(),
        keywords: words(&["font", "size", "text", "zoom"]),
        kind: ActionType::Singleton,
        parameters: vec![ActionParameter {
            id: "font_size".into(),
            name: "Font Size".into(),
            description: "Font size in pixels".into(),
            kind: ParameterType::Number,
            required: true,
            default: Some(ParamValue::Number(14.0)),
            options: vec![],
            validation: ParameterValidation {
                pattern: None,
                min: Some(8.0),
                max: Some(32.0),
            },
        }],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(FontSize),
    });

    reg.register(Action {
        id: "font-family".into(),
        title: "Change Font Family".into(),
        description: "Change the font family".into(),
        keywords: words(&["font", "family", "typeface"]),
        kind: ActionType::Singleton,
        parameters: vec![ActionParameter {
            id: "font_family".into(),
            name: "Font Family".into(),
            description: "Font family for the editor".into(),
            kind: ParameterType::Select,
            required: true,
            default: Some(ParamValue::Selected(vec!["monospace".into()])),
            options: font_family_options(),
            validation: ParameterValidation::default(),
        }],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(FontFamily),
    });

    reg.register(Action {
        id: "line-numbers".into(),
        title: "Toggle Line Numbers".into(),
        description: "Show or hide line numbers".into(),
        keywords: words(&["line", "numbers", "gutter"]),
        kind: ActionType::Singleton,
        parameters: vec![ActionParameter {
            id: "show_line_numbers".into(),
            name: "Show Line Numbers".into(),
            description: "Display line numbers in the editor".into(),
            kind: ParameterType::Boolean,
            required: true,
            default: Some(ParamValue::Boolean(true)),
            options: vec![],
            validation: ParameterValidation::default(),
        }],
        parent_id: Some("settings".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(LineNumbers),
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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(ToggleTheme),
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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(NodeOperations),
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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(Filter),
    });

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
            ActionParameter {
                id: "case_sensitive".into(),
                name: "Case Sensitive".into(),
                description: "Match case exactly".into(),
                kind: ParameterType::Boolean,
                required: false,
                default: Some(ParamValue::Boolean(false)),
                options: vec![],
                validation: ParameterValidation::default(),
            },
        ],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(FilterByName),
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
            ActionParameter {
                id: "case_sensitive".into(),
                name: "Case Sensitive".into(),
                description: "Match case exactly".into(),
                kind: ParameterType::Boolean,
                required: false,
                default: Some(ParamValue::Boolean(false)),
                options: vec![],
                validation: ParameterValidation::default(),
            },
        ],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(FilterByContent),
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
            // Seeded empty — populated dynamically from vault metadata
            // once a Tags-list pipeline is plumbed in.
            options: vec![],
            validation: ParameterValidation::default(),
        }],
        parent_id: Some("filter-actions".into()),
        children_ids: vec![],
        category: None,
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(FilterByTag),
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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(SearchNodes),
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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(CreateNode),
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
        contextual: false,
        shortcut: Some("F".into()),
        handler: ActionHandler::Builtin(FitCamera),
    });

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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(NewGraphTab),
    });

    reg.register(Action {
        id: "toggle-inspector".into(),
        title: "Toggle Inspector".into(),
        description: "Show or hide the right-hand inspector panel".into(),
        keywords: words(&["inspector", "sidebar", "panel", "toggle", "right"]),
        kind: ActionType::Singleton,
        parameters: vec![],
        parent_id: None,
        children_ids: vec![],
        category: Some("View".into()),
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(ToggleInspector),
    });

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
        contextual: false,
        shortcut: None,
        handler: ActionHandler::Builtin(ResetStyle),
    });

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
            contextual: false,
            shortcut: None,
            handler: ActionHandler::Builtin(JumpToSection(section)),
        });
    }
}

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
