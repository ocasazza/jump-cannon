<template>
  <div 
    class="sidebar-tab flex items-center justify-center w-12 h-12 rounded-md cursor-pointer transition-colors"
    :class="{ 'bg-accent bg-opacity-20': isActive }"
  >
    <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
    </svg>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  isActive: boolean;
}>();
</script>
