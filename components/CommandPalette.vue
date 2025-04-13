<template>
  <div class="fixed inset-0 bg-black bg-opacity-50 z-50 flex items-start justify-center pt-[20vh]">
    <div class="command-palette">
      <div class="p-2">
        <input 
          ref="inputRef"
          v-model="searchQuery" 
          type="text" 
          placeholder="Type a command or search..." 
          class="command-input"
          @keydown.down.prevent="selectNextItem"
          @keydown.up.prevent="selectPrevItem"
          @keydown.enter="executeSelected"
          @keydown.esc="close"
        />
      </div>
      
      <div class="command-results">
        <div v-if="filteredActions.length === 0" class="p-4 text-[var(--color-text-tertiary)] text-center">
          No matching actions found
        </div>
        <div v-else>
          <div 
            v-for="(action, index) in filteredActions" 
            :key="action.id"
            :class="[
              'command-item',
              selectedIndex === index ? 'active' : ''
            ]"
            @click="executeAction(action.id)"
            @mouseenter="selectedIndex = index"
          >
            <div class="flex-1">
              <div class="font-medium">{{ action.title }}</div>
              <div class="text-xs text-[var(--color-text-secondary)]">{{ action.description }}</div>
            </div>
            <div class="text-xs px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">
              {{ action.type === ActionType.SINGLETON ? 'Singleton' : 'Multi-instance' }}
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick } from 'vue';
import { useActionsStore, ActionType } from '~/stores/actions';

// Props and emits
const emit = defineEmits(['close']);

// Refs
const inputRef = ref<HTMLInputElement | null>(null);
const searchQuery = ref('');
const selectedIndex = ref(0);

// Get actions store
const actionsStore = useActionsStore();

// Computed properties
const filteredActions = computed(() => {
  const query = searchQuery.value.toLowerCase();
  if (!query) return actionsStore.getActions;
  
  return actionsStore.getActions.filter(action => {
    // Check title and description
    if (action.title.toLowerCase().includes(query) || 
        action.description.toLowerCase().includes(query)) {
      return true;
    }
    
    // Check keywords
    return action.keywords.some(keyword => 
      keyword.toLowerCase().includes(query)
    );
  });
});

// Methods
function selectNextItem() {
  if (filteredActions.value.length === 0) return;
  selectedIndex.value = (selectedIndex.value + 1) % filteredActions.value.length;
}

function selectPrevItem() {
  if (filteredActions.value.length === 0) return;
  selectedIndex.value = (selectedIndex.value - 1 + filteredActions.value.length) % filteredActions.value.length;
}

async function executeSelected() {
  if (filteredActions.value.length === 0) return;
  const selectedAction = filteredActions.value[selectedIndex.value];
  await executeAction(selectedAction.id);
}

async function executeAction(actionId: string) {
  try {
    await actionsStore.executeAction(actionId);
    close();
  } catch (error) {
    console.error('Failed to execute action:', error);
  }
}

function close() {
  emit('close');
}

// Focus input on mount
onMounted(async () => {
  await nextTick();
  inputRef.value?.focus();
});
</script>
