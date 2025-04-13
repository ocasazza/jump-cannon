<template>
  <div class="h-full">
    <NuxtLoadingIndicator />
    <NuxtLayout>
      <NuxtPage />
    </NuxtLayout>
  </div>
</template>

<script setup lang="ts">
import { useWasmStore } from '~/stores/wasm';
import { useThemeStore } from '~/stores/theme';

await callOnce('init-wasm', () => useWasmStore().initialize());
await callOnce('init-theme', () => useThemeStore().initialize());
</script>
