<template>
  <div class="flex flex-col h-screen">
    <!-- Main layout container -->
    <div class="flex flex-1 overflow-hidden">
      <!-- Left-positioned sidebar -->
      <template v-if="uiStore.sidebarPosition === 'left'">
        <!-- Activity Bar and Sidebar -->
        <div class="left-sidebar">
          <ActivityBar />
        </div>

        <!-- Editor Area (main content) -->
        <div class="editor-area overflow-y-auto flex-1">
          <slot />
        </div>
      </template>

      <!-- Right-positioned sidebar -->
      <template v-else>
        <!-- Editor Area (main content) -->
        <div class="editor-area overflow-y-auto flex-1">
          <slot />
        </div>

        <!-- Activity Bar and Sidebar -->
        <div class="right-sidebar">
          <ActivityBar />
        </div>
      </template>
    </div>

    <!-- Status Bar (bottom) -->
    <div class="status-bar">
      <div class="flex justify-between w-full">
        <div class="flex items-center gap-2">
          <span>jump-cannon</span>
          <span class="px-1">|</span>
          <button @click="themeStore.toggleTheme()" class="hover:text-text-primary dark:hover:text-text-dark-primary flex items-center gap-1">
            <svg v-if="themeStore.mode === 'light'" xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-3 h-3">
              <circle cx="12" cy="12" r="4"></circle>
              <path d="M12 2v2"></path>
              <path d="M12 20v2"></path>
              <path d="m4.93 4.93 1.41 1.41"></path>
              <path d="m17.66 17.66 1.41 1.41"></path>
              <path d="M2 12h2"></path>
              <path d="M20 12h2"></path>
              <path d="m6.34 17.66-1.41 1.41"></path>
              <path d="m19.07 4.93-1.41 1.41"></path>
            </svg>
            <svg v-else xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-3 h-3">
              <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"></path>
            </svg>
            {{ themeStore.mode === 'light' ? 'Light' : 'Dark' }}
          </button>
        </div>
        <div>
          <button @click="openCommandPalette" class="hover:text-text-primary dark:hover:text-text-dark-primary">
            Ctrl+P
          </button>
        </div>
      </div>
    </div>

    <!-- Command Palette is now managed in app.vue -->
  </div>
</template>

<script setup lang="ts">
import { onMounted } from 'vue';
import { useThemeStore } from '~/stores/theme';
import { useUIStore } from '~/stores/ui';
import ActivityBar from '~/components/sidebar/ActivityBar.vue';

// Stores
const themeStore = useThemeStore();
const uiStore = useUIStore();

// Command palette functions - now using the global app.vue instance
function openCommandPalette() {
  // Emit a global event that app.vue can listen for
  window.dispatchEvent(new CustomEvent('open-command-palette'));
}

// Initialize UI store
onMounted(() => {
  uiStore.initialize();
});
</script>
