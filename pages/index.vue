<template>
  <div class="p-6 h-full min-h-full">
    <div class="mb-6">
      <h1 class="text-2xl font-bold mb-2">KBG Command Palette</h1>
      <p class="text-text-secondary dark:text-text-dark-secondary mb-4">
        A VSCode-inspired UI with a command palette for executing actions
      </p>
      
      <div class="bg-bg-tertiary dark:bg-bg-dark-tertiary p-4 rounded mb-6">
        <p class="mb-2">Press <kbd class="bg-bg-secondary dark:bg-bg-dark-secondary px-2 py-1 rounded">Ctrl+P</kbd> or <kbd class="bg-bg-secondary dark:bg-bg-dark-secondary px-2 py-1 rounded">Cmd+P</kbd> to open the command palette</p>
        <p>Or click the <kbd class="bg-bg-secondary dark:bg-bg-dark-secondary px-2 py-1 rounded">Ctrl+P</kbd> button in the status bar</p>
      </div>
    </div>
    
    <div class="mb-6">
      <h2 class="text-xl font-bold mb-2">Available Actions</h2>
      <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div v-for="action in actions" :key="action.id" 
             class="bg-bg-secondary dark:bg-bg-dark-secondary p-4 rounded border border-border dark:border-border-dark hover:border-border-active dark:hover:border-border-dark-active cursor-pointer"
             @click="executeAction(action.id)">
          <div class="flex items-center justify-between mb-2">
            <h3 class="font-bold">{{ action.title }}</h3>
            <span v-if="action.type === ActionType.SINGLETON" class="text-xs px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">
              Singleton
            </span>
            <span v-else class="text-xs px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">
              Multi-instance
            </span>
          </div>
          <p class="text-sm text-text-secondary dark:text-text-dark-secondary">{{ action.description }}</p>
          <div class="mt-2 text-xs text-text-tertiary dark:text-text-dark-tertiary">
            Keywords: {{ action.keywords.join(', ') }}
          </div>
        </div>
      </div>
    </div>
    
    <div class="mb-6">
      <h2 class="text-xl font-bold mb-2">Active Instances</h2>
      <div v-if="activeInstances.length > 0" class="space-y-2">
        <div v-for="instance in activeInstances" :key="instance.id" 
             class="bg-bg-secondary dark:bg-bg-dark-secondary p-4 rounded border border-border dark:border-border-dark">
          <div class="flex items-center justify-between">
            <div>
              <h3 class="font-bold">{{ getActionTitle(instance.actionId) }}</h3>
              <pre class="mt-2 text-xs bg-bg-tertiary dark:bg-bg-dark-tertiary p-2 rounded overflow-x-auto">{{ JSON.stringify(instance.state, null, 2) }}</pre>
            </div>
            <button @click="removeInstance(instance.id)" 
                    class="text-error dark:text-error-dark hover:underline">
              Remove
            </button>
          </div>
        </div>
      </div>
      <div v-else class="bg-bg-secondary dark:bg-bg-dark-secondary p-4 rounded border border-border dark:border-border-dark text-text-tertiary dark:text-text-dark-tertiary">
        No active instances. Use the command palette to execute actions.
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted } from 'vue';
import { useWasmStore } from '~/stores/wasm';
import { useActionsStore, ActionType } from '~/stores/actions';

// Initialize WASM - wrapped in try/catch for safety
onMounted(() => {
  try {
    useWasmStore();
  } catch (error) {
    console.error('Failed to call WASM greet function:', error);
  }
});

// Get actions store
const actionsStore = useActionsStore();

// Computed properties
const actions = computed(() => actionsStore.getActions);
const activeInstances = computed(() => actionsStore.getActionInstances());

// Methods
function getActionTitle(actionId: string): string {
  const action = actions.value.find(a => a.id === actionId);
  return action ? action.title : 'Unknown Action';
}

async function executeAction(actionId: string) {
  try {
    await actionsStore.executeAction(actionId);
  } catch (error) {
    console.error('Failed to execute action:', error);
    // Could show an error message here
  }
}

function removeInstance(instanceId: string) {
  actionsStore.removeActionInstance(instanceId);
}
</script>
