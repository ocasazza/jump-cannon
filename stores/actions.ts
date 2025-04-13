import { defineStore } from 'pinia';
import { v4 as uuidv4 } from 'uuid';

// Action types
export enum ActionType {
  SINGLETON = 'singleton',
  MULTI_INSTANCE = 'multi-instance'
}

// Action interface
export interface Action {
  id: string;
  title: string;
  description: string;
  keywords: string[];
  type: ActionType;
  execute: (context?: any) => Promise<any>;
  isEnabled: () => boolean;
  isVisible: () => boolean;
}

// Action instance interface
export interface ActionInstance {
  id: string;
  actionId: string;
  state: any;
}

// Define the actions store
export const useActionsStore = defineStore('actions', {
  state: () => ({
    actions: [] as Action[],
    instances: [] as ActionInstance[]
  }),
  
  getters: {
    getActions: (state) => state.actions.filter(action => action.isVisible()),
    getEnabledActions: (state) => state.actions.filter(action => action.isEnabled() && action.isVisible()),
    getActionById: (state) => (id: string) => state.actions.find(action => action.id === id),
    getActionInstances: (state) => () => state.instances
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
    
    // Execute an action
    async executeAction(id: string, context?: any) {
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
          const result = await action.execute(context);
          existingInstance.state = result;
          return existingInstance;
        }
      }
      
      // Execute the action
      const result = await action.execute(context);
      
      // Create a new instance
      const instance: ActionInstance = {
        id: uuidv4(),
        actionId: id,
        state: result
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
    }
  }
});
