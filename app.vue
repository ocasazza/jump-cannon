<template>
  <SettingsProvider class="h-full">
    <NuxtLoadingIndicator />
    <NuxtLayout>
      <NuxtPage />
    </NuxtLayout>
    
    <!-- Global UI components -->
    <CommandPalette v-if="isCommandPaletteOpen" @close="closeCommandPalette" />
    
    <!-- Action parameter form when configuring outside of command palette -->
    <ActionParameterForm 
      v-if="configuringActionId && !isCommandPaletteOpen"
      :actionId="configuringActionId"
      @close="cancelConfiguring"
      @submit="finishConfiguring"
    />
  </SettingsProvider>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { useWasmStore } from '~/stores/wasm';
import { useThemeStore } from '~/stores/theme';
import { useActionsStore } from '~/stores/actions';
import { useWorkspaceStore } from '~/stores/workspace';
import CommandPalette from '~/components/CommandPalette.vue';
import ActionParameterForm from '~/components/actions/ActionParameterForm.vue';
import SettingsProvider from '~/components/SettingsProvider.vue';

// Initialize stores
await callOnce('init-wasm', () => useWasmStore().initialize());
await callOnce('init-theme', () => useThemeStore().initialize());
await callOnce('init-workspace', () => useWorkspaceStore().initialize());

// Command palette state
const isCommandPaletteOpen = ref(false);

// Get actions store
const actionsStore = useActionsStore();

// Computed properties
const configuringActionId = computed(() => {
  const configuring = actionsStore.getConfiguringAction;
  return configuring ? configuring.actionId : null;
});

// Methods
function openCommandPalette() {
  isCommandPaletteOpen.value = true;
}

function closeCommandPalette() {
  isCommandPaletteOpen.value = false;
}

function cancelConfiguring() {
  actionsStore.cancelConfiguring();
}

function finishConfiguring(params: Record<string, any>) {
  actionsStore.finishConfiguring(params);
}

// Global keyboard shortcuts
function handleKeyDown(event: KeyboardEvent) {
  // Command palette
  if ((event.ctrlKey || event.metaKey) && event.key === 'p') {
    event.preventDefault();
    openCommandPalette();
  }
}

// Listen for command palette open event from other components
function handleOpenCommandPalette() {
  openCommandPalette();
}

// Add global event listeners
onMounted(() => {
  window.addEventListener('keydown', handleKeyDown);
  window.addEventListener('open-command-palette', handleOpenCommandPalette);
});

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeyDown);
  window.removeEventListener('open-command-palette', handleOpenCommandPalette);
});
</script>
