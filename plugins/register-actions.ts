import { ActionType, type Action, type ActionParameter, useActionsStore } from '~/stores/actions';
import { useThemeStore } from '~/stores/theme';
import { useWorkspaceStore } from '~/stores/workspace';

// Example actions as mentioned in the UI/UX document
export default defineNuxtPlugin(() => {
  const actionsStore = useActionsStore();
  
  // ===== Settings Category =====
  
  // Settings parent action
  const settingsAction: Action = {
    id: 'settings',
    title: 'Settings',
    description: 'Configure application settings',
    keywords: ['settings', 'options', 'preferences', 'configure'],
    type: ActionType.SINGLETON,
    category: 'System',
    childrenIds: ['edit-options', 'toggle-theme', 'font-size', 'font-family', 'line-numbers'],
    execute: async () => {
      console.log('Executing Settings action');
      return { active: true };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register Edit Options action (singleton)
  const editOptionsAction: Action = {
    id: 'edit-options',
    title: 'Edit Options',
    description: 'Configure application settings',
    keywords: ['settings', 'options', 'preferences', 'configure'],
    type: ActionType.SINGLETON,
    parentId: 'settings',
    parameters: [
      {
        id: 'fontSize',
        name: 'Font Size',
        description: 'Font size in pixels',
        type: 'number',
        required: true,
        default: 14,
        validation: {
          min: 8,
          max: 32
        }
      },
      {
        id: 'fontFamily',
        name: 'Font Family',
        description: 'Font family for the editor',
        type: 'select',
        required: true,
        default: 'monospace',
        options: [
          { value: 'monospace', label: 'Monospace' },
          { value: 'sans-serif', label: 'Sans Serif' },
          { value: 'serif', label: 'Serif' }
        ]
      },
      {
        id: 'showLineNumbers',
        name: 'Show Line Numbers',
        description: 'Display line numbers in the editor',
        type: 'boolean',
        required: false,
        default: true
      }
    ],
    execute: async (params) => {
      console.log('Executing Edit Options action', params);
      
      // Use the workspace store to update settings
      const workspaceStore = useWorkspaceStore();
      if (params) {
        workspaceStore.updateSettings({
          fontSize: params.fontSize !== undefined ? params.fontSize : workspaceStore.settings.fontSize,
          fontFamily: params.fontFamily !== undefined ? params.fontFamily : workspaceStore.settings.fontFamily,
          showLineNumbers: params.showLineNumbers !== undefined ? params.showLineNumbers : workspaceStore.settings.showLineNumbers
        });
      }
      
      return {
        settings: workspaceStore.settings
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Font Size action
  const fontSizeAction: Action = {
    id: 'font-size',
    title: 'Change Font Size',
    description: 'Adjust the font size',
    keywords: ['font', 'size', 'text', 'zoom'],
    type: ActionType.SINGLETON,
    parentId: 'settings',
    parameters: [
      {
        id: 'fontSize',
        name: 'Font Size',
        description: 'Font size in pixels',
        type: 'number',
        required: true,
        default: 14,
        validation: {
          min: 8,
          max: 32
        }
      }
    ],
    execute: async (params) => {
      const workspaceStore = useWorkspaceStore();
      if (params?.fontSize !== undefined) {
        workspaceStore.updateSetting('fontSize', params.fontSize);
      }
      return {
        fontSize: workspaceStore.settings.fontSize
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };

  // Font Family action
  const fontFamilyAction: Action = {
    id: 'font-family',
    title: 'Change Font Family',
    description: 'Change the font family',
    keywords: ['font', 'family', 'typeface'],
    type: ActionType.SINGLETON,
    parentId: 'settings',
    parameters: [
      {
        id: 'fontFamily',
        name: 'Font Family',
        description: 'Font family for the editor',
        type: 'select',
        required: true,
        default: 'monospace',
        options: [
          { value: 'monospace', label: 'Monospace' },
          { value: 'sans-serif', label: 'Sans Serif' },
          { value: 'serif', label: 'Serif' }
        ]
      }
    ],
    execute: async (params) => {
      const workspaceStore = useWorkspaceStore();
      if (params?.fontFamily !== undefined) {
        workspaceStore.updateSetting('fontFamily', params.fontFamily);
      }
      return {
        fontFamily: workspaceStore.settings.fontFamily
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };

  // Line Numbers action
  const lineNumbersAction: Action = {
    id: 'line-numbers',
    title: 'Toggle Line Numbers',
    description: 'Show or hide line numbers',
    keywords: ['line', 'numbers', 'gutter'],
    type: ActionType.SINGLETON,
    parentId: 'settings',
    parameters: [
      {
        id: 'showLineNumbers',
        name: 'Show Line Numbers',
        description: 'Display line numbers in the editor',
        type: 'boolean',
        required: true,
        default: true
      }
    ],
    execute: async (params) => {
      const workspaceStore = useWorkspaceStore();
      if (params?.showLineNumbers !== undefined) {
        workspaceStore.updateSetting('showLineNumbers', params.showLineNumbers);
      }
      return {
        showLineNumbers: workspaceStore.settings.showLineNumbers
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Toggle Theme action (singleton)
  const toggleThemeAction: Action = {
    id: 'toggle-theme',
    title: 'Toggle Theme',
    description: 'Switch between light and dark themes',
    keywords: ['theme', 'dark', 'light', 'toggle', 'switch'],
    type: ActionType.SINGLETON,
    parentId: 'settings',
    execute: async () => {
      const themeStore = useThemeStore();
      themeStore.toggleTheme();
      return { theme: themeStore.mode };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // ===== Node Operations Category =====
  
  // Node Operations parent action
  const nodeOperationsAction: Action = {
    id: 'node-operations',
    title: 'Node Operations',
    description: 'Operations for working with nodes',
    keywords: ['node', 'operations', 'actions'],
    type: ActionType.SINGLETON,
    category: 'Nodes',
    childrenIds: ['filter', 'search-nodes', 'create-node'],
    execute: async () => {
      console.log('Executing Node Operations action');
      return { active: true };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Filter parent action
  const filterAction: Action = {
    id: 'filter',
    title: 'Filter',
    description: 'Apply filters to nodes',
    keywords: ['filter', 'search', 'find'],
    type: ActionType.SINGLETON,
    parentId: 'node-operations',
    childrenIds: ['filter-by-name', 'filter-by-content', 'filter-by-tag'],
    execute: async () => {
      console.log('Executing Filter action');
      return { active: true };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Filter by Name action
  const filterByNameAction: Action = {
    id: 'filter-by-name',
    title: 'Filter by Name',
    description: 'Filter nodes by name',
    keywords: ['filter', 'name'],
    type: ActionType.MULTI_INSTANCE,
    parentId: 'filter',
    parameters: [
      {
        id: 'pattern',
        name: 'Name Pattern',
        description: 'Pattern to match node names (supports * wildcard)',
        type: 'string',
        required: true,
        default: '*'
      },
      {
        id: 'caseSensitive',
        name: 'Case Sensitive',
        description: 'Match case exactly',
        type: 'boolean',
        required: false,
        default: false
      }
    ],
    execute: async (params) => {
      console.log('Executing Filter by Name action', params);
      return {
        filter: {
          type: 'name',
          pattern: params?.pattern || '*',
          caseSensitive: params?.caseSensitive || false
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Filter by Content action
  const filterByContentAction: Action = {
    id: 'filter-by-content',
    title: 'Filter by Content',
    description: 'Filter nodes by content',
    keywords: ['filter', 'content'],
    type: ActionType.MULTI_INSTANCE,
    parentId: 'filter',
    parameters: [
      {
        id: 'pattern',
        name: 'Content Pattern',
        description: 'Pattern to match node content',
        type: 'string',
        required: true,
        default: ''
      },
      {
        id: 'caseSensitive',
        name: 'Case Sensitive',
        description: 'Match case exactly',
        type: 'boolean',
        required: false,
        default: false
      }
    ],
    execute: async (params) => {
      console.log('Executing Filter by Content action', params);
      return {
        filter: {
          type: 'content',
          pattern: params?.pattern || '',
          caseSensitive: params?.caseSensitive || false
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Filter by Tag action
  const filterByTagAction: Action = {
    id: 'filter-by-tag',
    title: 'Filter by Tag',
    description: 'Filter nodes by tag',
    keywords: ['filter', 'tag'],
    type: ActionType.MULTI_INSTANCE,
    parentId: 'filter',
    parameters: [
      {
        id: 'tags',
        name: 'Tags',
        description: 'Tags to filter by',
        type: 'multiselect',
        required: true,
        default: [],
        options: [
          { value: 'important', label: 'Important' },
          { value: 'draft', label: 'Draft' },
          { value: 'archived', label: 'Archived' },
          { value: 'shared', label: 'Shared' }
        ]
      }
    ],
    execute: async (params) => {
      console.log('Executing Filter by Tag action', params);
      return {
        filter: {
          type: 'tag',
          tags: params?.tags || []
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
    parentId: 'node-operations',
    parameters: [
      {
        id: 'query',
        name: 'Search Query',
        description: 'Text to search for',
        type: 'string',
        required: true,
        default: ''
      },
      {
        id: 'scope',
        name: 'Search Scope',
        description: 'Where to search',
        type: 'select',
        required: true,
        default: 'all',
        options: [
          { value: 'all', label: 'All Nodes' },
          { value: 'selected', label: 'Selected Nodes' },
          { value: 'visible', label: 'Visible Nodes' }
        ]
      },
      {
        id: 'includeContent',
        name: 'Include Content',
        description: 'Search in node content',
        type: 'boolean',
        required: false,
        default: true
      }
    ],
    execute: async (params) => {
      console.log('Executing Search Nodes action', params);
      return {
        search: {
          query: params?.query || '',
          scope: params?.scope || 'all',
          includeContent: params?.includeContent !== undefined ? params.includeContent : true
        }
      };
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
    parentId: 'node-operations',
    parameters: [
      {
        id: 'name',
        name: 'Node Name',
        description: 'Name of the new node',
        type: 'string',
        required: true,
        default: 'New Node'
      },
      {
        id: 'type',
        name: 'Node Type',
        description: 'Type of node to create',
        type: 'select',
        required: true,
        default: 'default',
        options: [
          { value: 'default', label: 'Default' },
          { value: 'text', label: 'Text' },
          { value: 'image', label: 'Image' },
          { value: 'code', label: 'Code' }
        ]
      },
      {
        id: 'tags',
        name: 'Tags',
        description: 'Tags to apply to the node',
        type: 'multiselect',
        required: false,
        default: [],
        options: [
          { value: 'important', label: 'Important' },
          { value: 'draft', label: 'Draft' },
          { value: 'archived', label: 'Archived' },
          { value: 'shared', label: 'Shared' }
        ]
      }
    ],
    execute: async (params) => {
      console.log('Executing Create Node action', params);
      return {
        node: {
          id: `node-${Date.now()}`,
          type: params?.type || 'default',
          name: params?.name || 'New Node',
          tags: params?.tags || []
        }
      };
    },
    isEnabled: () => true,
    isVisible: () => true
  };
  
  // Register all actions
  actionsStore.registerAction(settingsAction);
  actionsStore.registerAction(editOptionsAction);
  actionsStore.registerAction(toggleThemeAction);
  actionsStore.registerAction(fontSizeAction);
  actionsStore.registerAction(fontFamilyAction);
  actionsStore.registerAction(lineNumbersAction);
  actionsStore.registerAction(nodeOperationsAction);
  actionsStore.registerAction(filterAction);
  actionsStore.registerAction(filterByNameAction);
  actionsStore.registerAction(filterByContentAction);
  actionsStore.registerAction(filterByTagAction);
  actionsStore.registerAction(searchNodesAction);
  actionsStore.registerAction(createNodeAction);
  
  return {
    provide: {
      // If needed, provide something to the app
    }
  };
});
