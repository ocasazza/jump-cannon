import { ActionType, type Action, useActionsStore } from '~/stores/actions';
import { useThemeStore } from '~/stores/theme';

// Example actions as mentioned in the UI/UX document
export default defineNuxtPlugin(({ $pinia }) => {
  const actionsStore = useActionsStore();
  
  // Register Edit Options action (singleton)
  const editOptionsAction: Action = {
    id: 'edit-options',
    title: 'Edit Options',
    description: 'Configure application settings',
    keywords: ['settings', 'options', 'preferences', 'configure'],
    type: ActionType.SINGLETON,
    execute: async (context) => {
      console.log('Executing Edit Options action', context);
      // In a real implementation, this would open a settings panel or modal
      return {
        settings: {
          theme: 'dark',
          fontSize: 14,
          // Other settings...
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register Filter Nodes action (multi-instance)
  const filterNodesAction: Action = {
    id: 'filter-nodes',
    title: 'Filter Nodes',
    description: 'Filter nodes based on criteria',
    keywords: ['filter', 'nodes', 'criteria', 'search'],
    type: ActionType.MULTI_INSTANCE,
    execute: async (context) => {
      console.log('Executing Filter Nodes action', context);
      // In a real implementation, this would create a new filter
      return {
        filter: {
          type: 'name',
          pattern: '*',
          caseSensitive: false
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register Search Nodes action (multi-instance)
  const searchNodesAction: Action = {
    id: 'search-nodes',
    title: 'Search Nodes',
    description: 'Search for nodes by name or content',
    keywords: ['search', 'find', 'nodes', 'query'],
    type: ActionType.MULTI_INSTANCE,
    execute: async (context) => {
      console.log('Executing Search Nodes action', context);
      // In a real implementation, this would open a search panel
      return {
        search: {
          query: '',
          scope: 'all',
          includeContent: true
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register additional example actions
  
  // Toggle Theme action (singleton)
  const toggleThemeAction: Action = {
    id: 'toggle-theme',
    title: 'Toggle Theme',
    description: 'Switch between light and dark themes',
    keywords: ['theme', 'dark', 'light', 'toggle', 'switch'],
    type: ActionType.SINGLETON,
    execute: async () => {
      const themeStore = useThemeStore();
      themeStore.toggleTheme();
      return { theme: themeStore.mode };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Create New Node action (multi-instance)
  const createNodeAction: Action = {
    id: 'create-node',
    title: 'Create New Node',
    description: 'Create a new node in the workspace',
    keywords: ['create', 'new', 'node', 'add'],
    type: ActionType.MULTI_INSTANCE,
    execute: async (context) => {
      console.log('Executing Create Node action', context);
      // In a real implementation, this would create a new node
      return {
        node: {
          id: `node-${Date.now()}`,
          type: 'default',
          name: 'New Node'
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register all actions
  actionsStore.registerAction(editOptionsAction);
  actionsStore.registerAction(filterNodesAction);
  actionsStore.registerAction(searchNodesAction);
  actionsStore.registerAction(toggleThemeAction);
  actionsStore.registerAction(createNodeAction);
  
  return {
    provide: {
      // If needed, provide something to the app
    }
  };
});
