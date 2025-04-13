<template>
  <div class="activity-bar-icon" :class="{ active: isActive }">
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path>
      <polyline points="14 2 14 8 20 8"></polyline>
      <line x1="12" y1="18" x2="12" y2="12"></line>
      <line x1="9" y1="15" x2="15" y2="15"></line>
    </svg>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  isActive: boolean;
}>();
</script>

<style scoped>
.activity-bar-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  color: var(--text-tertiary);
  cursor: pointer;
  transition: color 0.2s, background-color 0.2s;
}

.activity-bar-icon:hover {
  color: var(--text-secondary);
  background-color: var(--bg-tertiary);
}

.activity-bar-icon.active {
  color: var(--text-primary);
  border-left: 2px solid var(--accent);
}

:deep(.right-sidebar) .activity-bar-icon.active {
  border-left: none;
  border-right: 2px solid var(--accent);
}
</style>
