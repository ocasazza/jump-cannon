<template>
  <div class="h-full overflow-y-auto">
    <div class="p-4">
      <h2 class="text-lg font-bold mb-3">Actions Information</h2>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">About Actions</h3>
        <p class="text-sm mb-2">
          Actions are commands that can be executed through the command palette.
          They provide a generic interface for interacting with the application.
        </p>
        <p class="text-sm mb-2">
          Press <kbd class="bg-[var(--color-bg-tertiary)] px-1 py-0.5 rounded text-xs">Ctrl+P</kbd> 
          or <kbd class="bg-[var(--color-bg-tertiary)] px-1 py-0.5 rounded text-xs">Cmd+P</kbd> 
          to open the command palette.
        </p>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">Action Types</h3>
        <div class="text-sm mb-2">
          <div class="flex items-center gap-2 mb-1">
            <span class="text-xs px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">Singleton</span>
            <span>Can only be applied once per workspace</span>
          </div>
          <div class="flex items-center gap-2">
            <span class="text-xs px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">Multi-instance</span>
            <span>Can be applied multiple times</span>
          </div>
        </div>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">Available Actions</h3>
        <div v-for="action in actions" :key="action.id" class="mb-3 border-b border-[var(--color-border)] pb-2">
          <div class="flex items-center justify-between mb-1">
            <span class="font-medium">{{ action.title }}</span>
            <span class="text-xs px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">
              {{ action.type === ActionType.SINGLETON ? 'Singleton' : 'Multi-instance' }}
            </span>
          </div>
          <p class="text-xs text-[var(--color-text-secondary)] mb-1">{{ action.description }}</p>
          <div class="text-xs text-[var(--color-text-tertiary)]">
            Keywords: {{ action.keywords.join(', ') }}
          </div>
        </div>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">Active Instances</h3>
        <div v-if="activeInstances.length > 0">
          <div v-for="instance in activeInstances" :key="instance.id" class="mb-2 text-sm">
            <div class="font-medium">{{ getActionTitle(instance.actionId) }}</div>
            <pre class="text-xs bg-[var(--color-bg-tertiary)] p-1 rounded overflow-x-auto mt-1">{{ JSON.stringify(instance.state, null, 2) }}</pre>
          </div>
        </div>
        <div v-else class="text-sm text-[var(--color-text-tertiary)]">
          No active instances. Use the command palette to execute actions.
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import { useActionsStore, ActionType } from '~/stores/actions';

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
</script>
