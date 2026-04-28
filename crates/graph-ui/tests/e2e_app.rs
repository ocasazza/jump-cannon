mod e2e {
    use bevy::prelude::*;
    use graph_ui::state::ui::{UiState, SidebarTab};
    use graph_ui::actions::ActionRegistry;

    fn base_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<UiState>();
        app.init_resource::<ActionRegistry>();
        app
    }

    #[test]
    fn ui_state_defaults() {
        let mut app = base_app();
        app.update();
        let state = app.world().resource::<UiState>();
        assert!(!state.palette_open);
        assert!(state.sidebar_width > 0.0);
        assert_eq!(state.active_tab, SidebarTab::Search);
    }

    #[test]
    fn action_registry_starts_empty_without_startup_system() {
        let mut app = base_app();
        app.update();
        let registry = app.world().resource::<ActionRegistry>();
        // Without the register_actions startup system, registry has no actions
        assert_eq!(registry.actions.len(), 0);
    }

    #[test]
    fn action_registry_populated_with_startup_system() {
        use graph_ui::register_actions;
        let mut app = base_app();
        app.add_systems(Startup, register_actions);
        app.update(); // runs Startup then Update
        let registry = app.world().resource::<ActionRegistry>();
        assert!(registry.actions.len() >= 5, "expected at least 5 built-in actions");
    }

    #[test]
    fn palette_toggle_via_resource_mutation() {
        let mut app = base_app();
        app.update();
        // Simulate what the input system does
        app.world_mut().resource_mut::<UiState>().palette_open = true;
        app.update();
        assert!(app.world().resource::<UiState>().palette_open);
        app.world_mut().resource_mut::<UiState>().palette_open = false;
        app.update();
        assert!(!app.world().resource::<UiState>().palette_open);
    }
}
