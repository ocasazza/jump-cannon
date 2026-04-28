import { defineStore } from 'pinia';
import { v4 as uuidv4 } from 'uuid';

// Action types
export enum ActionType {
  SINGLETON = 'singleton',
  MULTI_INSTANCE = 'multi-instance'
}

// Parameter types
export type ParameterType = 'string' | 'number' | 'boolean' | 'select' | 'multiselect';

// Parameter definition interface
export interface ActionParameter {
  id: string;
  name: string;
  description: string;
  type: ParameterType;
  required: boolean;
  default?: any;
  options?: Array<{value: any, label: string}>; // For select/multiselect types
  validation?: {
    pattern?: string;
    min?: number;
    max?: number;
    // Other validation rules as needed
  };
}

// Action interface
export interface Action {
  id: string;
  title: string;
  description: string;
  keywords: string[];
  type: ActionType;
  execute: (params?: Record<string, any>) => Promise<any>;
  isEnabled: () => boolean;
  isVisible: () => boolean;
  
  // New properties
  parameters?: ActionParameter[];  // Optional parameters definition
  parentId?: string;               // Optional parent action ID
  childrenIds?: string[];          // Optional child action IDs
  category?: string;               // Optional category for top-level grouping
  contextual?: boolean;            // Whether this action only appears in context of parent
}

// Action instance interface
export interface ActionInstance {
  id: string;
  actionId: string;
  state: any;
  params?: Record<string, any>;    // Parameters used to execute the action
}

// Define the actions store
export const useActionsStore = defineStore('actions', {
  state: () => ({
    actions: [] as Action[],
    instances: [] as ActionInstance[],
    configuring: null as { actionId: string, callback: (params: Record<string, any>) => void } | null
  }),
  
  getters: {
    getActions: (state) => state.actions.filter(action => action.isVisible()),
    getEnabledActions: (state) => state.actions.filter(action => action.isEnabled() && action.isVisible()),
    getActionById: (state) => (id: string) => state.actions.find(action => action.id === id),
    getActionInstances: (state) => () => state.instances,
    
    // Get root-level actions (no parent)
    getRootActions: (state) => 
      state.actions.filter(action => !action.parentId && action.isVisible()),
    
    // Get child actions for a given parent ID
    getChildActions: (state) => (parentId: string) => 
      state.actions.filter(action => action.parentId === parentId && action.isVisible()),
    
    // Get action categories
    getCategories: (state) => {
      const categories = new Set<string>();
      state.actions.forEach(action => {
        if (action.category) categories.add(action.category);
      });
      return Array.from(categories);
    },
    
    // Get actions by category
    getActionsByCategory: (state) => (category: string) =>
      state.actions.filter(action => action.category === category && action.isVisible()),
      
    // Get action being configured
    getConfiguringAction: (state) => state.configuring
  },
  
  actions: {
    // Register a new action
    registerAction(action: Action) {
      // Check if action with this ID already exists
      const existingIndex = this.actions.findIndex(a => a.id === action.id);
      if (existingIndex >= 0) {
        // Replace existing action
        this.actions.splice(existingIndex, 1, action);
      } else {
        // Add new action
        this.actions.push(action);
      }
    },
    
    // Unregister an action
    unregisterAction(id: string) {
      const index = this.actions.findIndex(action => action.id === id);
      if (index >= 0) {
        this.actions.splice(index, 1);
        
        // Remove any instances of this action
        this.instances = this.instances.filter(instance => instance.actionId !== id);
      }
    },
    
    // Start configuring an action
    startConfiguring(actionId: string, callback: (params: Record<string, any>) => void) {
      this.configuring = { actionId, callback };
    },
    
    // Finish configuring an action
    finishConfiguring(params: Record<string, any>) {
      if (this.configuring) {
        const { callback } = this.configuring;
        this.configuring = null;
        callback(params);
      }
    },
    
    // Cancel configuring an action
    cancelConfiguring() {
      this.configuring = null;
    },
    
    // Execute an action
    async executeAction(id: string, params?: Record<string, any>) {
      const action = this.getActionById(id);
      if (!action) {
        throw new Error(`Action with ID ${id} not found`);
      }
      
      if (!action.isEnabled()) {
        throw new Error(`Action with ID ${id} is not enabled`);
      }
      
      // For singleton actions, check if an instance already exists
      if (action.type === ActionType.SINGLETON) {
        const existingInstance = this.instances.find(instance => instance.actionId === id);
        if (existingInstance) {
          // Update existing instance
          const result = await action.execute(params);
          existingInstance.state = result;
          existingInstance.params = params;
          return existingInstance;
        }
      }
      
      // Execute the action
      const result = await action.execute(params);
      
      // Create a new instance
      const instance: ActionInstance = {
        id: uuidv4(),
        actionId: id,
        state: result,
        params
      };
      
      // Add the instance to the store
      this.instances.push(instance);
      
      return instance;
    },
    
    // Remove an action instance
    removeActionInstance(instanceId: string) {
      const index = this.instances.findIndex(instance => instance.id === instanceId);
      if (index >= 0) {
        this.instances.splice(index, 1);
      }
    },
    
    // Update an existing action instance
    async updateActionInstance(instanceId: string, params: Record<string, any>) {
      const instance = this.instances.find(instance => instance.id === instanceId);
      if (!instance) {
        throw new Error(`Action instance with ID ${instanceId} not found`);
      }
      
      const action = this.getActionById(instance.actionId);
      if (!action) {
        throw new Error(`Action with ID ${instance.actionId} not found`);
      }
      
      if (!action.isEnabled()) {
        throw new Error(`Action with ID ${instance.actionId} is not enabled`);
      }
      
      // Execute the action with the new parameters
      const result = await action.execute(params);
      
      // Update the instance
      instance.state = result;
      instance.params = params;
      
      return instance;
    },
    
    // Add a child action to a parent
    addChildAction(parentId: string, childId: string) {
      const parent = this.getActionById(parentId);
      const child = this.getActionById(childId);
      
      if (parent && child) {
        // Update parent's children list
        if (!parent.childrenIds) parent.childrenIds = [];
        if (!parent.childrenIds.includes(childId)) {
          parent.childrenIds.push(childId);
        }
        
        // Update child's parent reference
        child.parentId = parentId;
      }
    },
    
    // Remove a child action from a parent
    removeChildAction(parentId: string, childId: string) {
      const parent = this.getActionById(parentId);
      const child = this.getActionById(childId);
      
      if (parent && child) {
        // Update parent's children list
        if (parent.childrenIds) {
          parent.childrenIds = parent.childrenIds.filter(id => id !== childId);
        }
        
        // Update child's parent reference
        if (child.parentId === parentId) {
          delete child.parentId;
        }
      }
    }
  }
});
