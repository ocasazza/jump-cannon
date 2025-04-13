<template>
  <div class="h-full">
    <NuxtLoadingIndicator />
    <NuxtLayout>
      <NuxtPage />
    </NuxtLayout>
  </div>
</template>

<script setup lang="ts">
import { onMounted } from 'vue';
import { useWasmStore } from '~/stores/wasm';
import { useThemeStore } from '~/stores/theme';

// Initialize WASM - uncommented but wrapped in try/catch for safety
try {
  const { init, isLoaded } = useWasmStore();
  await callOnce('init', () => init());
} catch (error) {
  console.error('Failed to initialize WASM:', error);
}

// Initialize theme
const themeStore = useThemeStore();
onMounted(() => {
  themeStore.initialize();
});
</script>
