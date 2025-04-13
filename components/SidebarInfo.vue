<template>
  <div class="h-full overflow-y-auto">
    <div class="p-4">
      <h2 class="text-lg font-bold mb-3">Actions Information</h2>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">About Actions</h3>
        <p class="text-sm mb-2">
          Actions are commands that can be executed through the command palette.
          They provide a generic interface for interacting with the application.
        </p>
        <p class="text-sm mb-2">
          Press <kbd class="bg-bg-tertiary dark:bg-bg-dark-tertiary px-1 py-0.5 rounded text-xs">Ctrl+P</kbd> 
          or <kbd class="bg-bg-tertiary dark:bg-bg-dark-tertiary px-1 py-0.5 rounded text-xs">Cmd+P</kbd> 
          to open the command palette.
        </p>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Action Types</h3>
        <div class="text-sm mb-2">
          <div class="flex items-center gap-2 mb-1">
            <span class="text-xs px-1.5 py-0.5 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">Singleton</span>
            <span>Can only be applied once per workspace</span>
          </div>
          <div class="flex items-center gap-2">
            <span class="text-xs px-1.5 py-0.5 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">Multi-instance</span>
            <span>Can be applied multiple times</span>
          </div>
        </div>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Action Categories</h3>
        <div v-for="category in categories" :key="category" class="mb-3">
          <div class="font-medium mb-1">{{ category }}</div>
          <div v-for="action in getActionsByCategory(category)" :key="action.id" 
               class="ml-2 mb-2 border-l-2 border-border dark:border-border-dark pl-2">
            <div class="flex items-center justify-between">
              <span class="font-medium text-sm">{{ action.title }}</span>
              <span class="text-xs px-1.5 py-0.5 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">
                {{ action.type === ActionType.SINGLETON ? 'Singleton' : 'Multi-instance' }}
              </span>
            </div>
            <p class="text-xs text-text-secondary dark:text-text-dark-secondary">{{ action.description }}</p>
            <div v-if="action.parameters && action.parameters.length > 0" class="mt-1">
              <div class="text-xs text-text-tertiary dark:text-text-dark-tertiary">
                Parameters: {{ action.parameters.length }}
              </div>
            </div>
          </div>
        </div>
      </div>
      
      <div class="mb-4">
        <div class="flex items-center justify-between mb-2">
          <h3 class="text-sm font-bold text-text-secondary dark:text-text-dark-secondary">Active Instances</h3>
          <div class="flex space-x-2">
            <button 
              @click="activeFilter = 'all'" 
              :class="[
                'text-xs px-2 py-1 rounded',
                activeFilter === 'all' 
                  ? 'bg-accent dark:bg-accent-dark text-white' 
                  : 'bg-bg-tertiary dark:bg-bg-dark-tertiary'
              ]"
            >
              All
            </button>
            <button 
              @click="activeFilter = 'singleton'" 
              :class="[
                'text-xs px-2 py-1 rounded',
                activeFilter === 'singleton' 
                  ? 'bg-accent dark:bg-accent-dark text-white' 
                  : 'bg-bg-tertiary dark:bg-bg-dark-tertiary'
              ]"
            >
              Singleton
            </button>
            <button 
              @click="activeFilter = 'multi-instance'" 
              :class="[
                'text-xs px-2 py-1 rounded',
                activeFilter === 'multi-instance' 
                  ? 'bg-accent dark:bg-accent-dark text-white' 
                  : 'bg-bg-tertiary dark:bg-bg-dark-tertiary'
              ]"
            >
              Multi
            </button>
          </div>
        </div>
        
        <ActionList :filter="activeFilter" />
      </div>
      
      <!-- Action being configured -->
      <div v-if="configuringAction" class="mb-4 p-3 bg-bg-secondary dark:bg-bg-dark-secondary rounded">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Configuring Action</h3>
        <div class="font-medium">{{ configuringAction.title }}</div>
        <p class="text-xs text-text-secondary dark:text-text-dark-secondary">{{ configuringAction.description }}</p>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { useActionsStore, ActionType } from '~/stores/actions';
import ActionList from '~/components/actions/ActionList.vue';

// Get actions store
const actionsStore = useActionsStore();

// Refs
const activeFilter = ref('all');

// Computed properties
const actions = computed(() => actionsStore.getActions);
const categories = computed(() => actionsStore.getCategories);
const configuringAction = computed(() => {
  const configuring = actionsStore.getConfiguringAction;
  if (!configuring) return null;
  
  return actionsStore.getActionById(configuring.actionId);
});

// Methods
function getActionsByCategory(category: string) {
  return actionsStore.getActionsByCategory(category);
}
</script>
