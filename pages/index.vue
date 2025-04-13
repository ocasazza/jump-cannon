<template>
  <div class="p-6 h-full min-h-full">
  </div>
</template>

<script setup lang="ts">
import { onMounted } from 'vue';
import { useWasmStore } from '~/stores/wasm';

// Initialize WASM - wrapped in try/catch for safety
onMounted(() => {
  try {
    useWasmStore();
  } catch (error) {
    console.error('Failed to call WASM greet function:', error);
  }
});
</script>
