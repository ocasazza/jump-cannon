<template>
  <div :class="rootClasses">
    <slot></slot>
  </div>
</template>

<script setup lang="ts">
import { computed, watch, onMounted } from 'vue';
import { useWorkspaceStore } from '~/stores/workspace';

// Get workspace store
const workspaceStore = useWorkspaceStore();

// Computed classes based on settings
const rootClasses = computed(() => {
  const classes = [];
  
  // Font size classes
  const fontSize = workspaceStore.settings.fontSize;
  if (fontSize) {
    // Map font size to closest Tailwind size
    if (fontSize <= 10) classes.push('text-xs');
    else if (fontSize <= 12) classes.push('text-sm');
    else if (fontSize <= 14) classes.push('text-base');
    else if (fontSize <= 16) classes.push('text-lg');
    else if (fontSize <= 18) classes.push('text-xl');
    else if (fontSize <= 20) classes.push('text-2xl');
    else classes.push('text-3xl');
  }
  
  // Font family classes
  const fontFamily = workspaceStore.settings.fontFamily;
  if (fontFamily === 'monospace') classes.push('font-mono');
  else if (fontFamily === 'sans-serif') classes.push('font-sans');
  else if (fontFamily === 'serif') classes.push('font-serif');
  
  return classes;
});

// Apply line numbers setting via CSS variable
function applyLineNumbersSetting() {
  const showLineNumbers = workspaceStore.settings.showLineNumbers;
  document.documentElement.style.setProperty(
    '--show-line-numbers', 
    showLineNumbers ? 'block' : 'none'
  );
}

// Watch for settings changes
watch(() => workspaceStore.settings, () => {
  applyLineNumbersSetting();
}, { deep: true });

// Apply settings on mount
onMounted(() => {
  applyLineNumbersSetting();
});
</script>

<style>
:root {
  --show-line-numbers: block;
}

/* This class can be used in components that display code */
.line-numbers {
  display: var(--show-line-numbers);
}
</style>
