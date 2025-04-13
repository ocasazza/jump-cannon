<template>
  <div class="flex flex-col h-screen">
    <!-- Main layout container -->
    <div class="flex flex-1 overflow-hidden">
      <!-- Activity Bar (left sidebar with icons) -->
      <div class="activity-bar">
        <div class="flex flex-col gap-4 pt-2">
          <button 
            @click="setActiveTab('explorer')" 
            :class="[
              'w-8 h-8 flex items-center justify-center hover:text-[var(--color-text-primary)]',
              activeTab === 'explorer' 
                ? 'text-[var(--color-text-primary)] border-l-2 border-[var(--color-accent)]' 
                : 'text-[var(--color-text-secondary)]'
            ]"
            title="Explorer"
          >
            <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-5 h-5">
              <path d="M3 9h18v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9Z"></path>
              <path d="M3 9V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v4"></path>
            </svg>
          </button>
          <button 
            @click="setActiveTab('search')" 
            :class="[
              'w-8 h-8 flex items-center justify-center hover:text-[var(--color-text-primary)]',
              activeTab === 'search' 
                ? 'text-[var(--color-text-primary)] border-l-2 border-[var(--color-accent)]' 
                : 'text-[var(--color-text-secondary)]'
            ]"
            title="Search"
          >
            <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-5 h-5">
              <circle cx="11" cy="11" r="8"></circle>
              <path d="m21 21-4.3-4.3"></path>
            </svg>
          </button>
          <button 
            @click="setActiveTab('info')" 
            :class="[
              'w-8 h-8 flex items-center justify-center hover:text-[var(--color-text-primary)]',
              activeTab === 'info' 
                ? 'text-[var(--color-text-primary)] border-l-2 border-[var(--color-accent)]' 
                : 'text-[var(--color-text-secondary)]'
            ]"
            title="Actions Info"
          >
            <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-5 h-5">
              <circle cx="12" cy="12" r="10"></circle>
              <path d="M12 16v-4"></path>
              <path d="M12 8h.01"></path>
            </svg>
          </button>
          <button 
            @click="setActiveTab('settings')" 
            :class="[
              'w-8 h-8 flex items-center justify-center hover:text-[var(--color-text-primary)]',
              activeTab === 'settings' 
                ? 'text-[var(--color-text-primary)] border-l-2 border-[var(--color-accent)]' 
                : 'text-[var(--color-text-secondary)]'
            ]"
            title="Settings"
          >
            <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-5 h-5">
              <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"></path>
            </svg>
          </button>
        </div>
      </div>

      <!-- Side Bar (explorer, search, etc.) -->
      <div v-if="isSidebarVisible" class="side-bar h-full flex flex-col">
        <div class="flex-1 overflow-hidden">
          <!-- Explorer Tab -->
          <SidebarExplorer v-if="activeTab === 'explorer'" />
          
          <!-- Search Tab (placeholder) -->
          <div v-else-if="activeTab === 'search'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Search</h2>
            <div class="mb-4">
              <input 
                type="text" 
                placeholder="Search..." 
                class="w-full p-2 bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] rounded"
              />
            </div>
            <div class="text-sm text-[var(--color-text-tertiary)]">
              Type to search in files
            </div>
          </div>
          
          <!-- Info Tab -->
          <SidebarInfo v-else-if="activeTab === 'info'" />
          
          <!-- Settings Tab (placeholder) -->
          <div v-else-if="activeTab === 'settings'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Settings</h2>
            <div class="mb-4">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Theme</span>
                <button 
                  @click="toggleTheme" 
                  class="px-2 py-1 bg-[var(--color-bg-tertiary)] rounded text-sm"
                >
                  {{ themeStore.mode === 'light' ? 'Light' : 'Dark' }}
                </button>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Sidebar Position</span>
                <button 
                  @click="toggleSidebarPosition" 
                  class="px-2 py-1 bg-[var(--color-bg-tertiary)] rounded text-sm"
                >
                  Left
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- Editor Area (main content) -->
      <div class="editor-area overflow-y-auto">
        <slot />
      </div>
    </div>

    <!-- Status Bar (bottom) -->
    <div class="status-bar">
      <div class="flex justify-between w-full">
        <div class="flex items-center gap-2">
          <span>KBG</span>
          <span class="px-1">|</span>
          <button @click="toggleTheme" class="hover:text-[var(--color-text-primary)] flex items-center gap-1">
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
          <button @click="openCommandPalette" class="hover:text-[var(--color-text-primary)]">
            Ctrl+P
          </button>
        </div>
      </div>
    </div>

    <!-- Command Palette (hidden by default) -->
    <CommandPalette v-if="isCommandPaletteOpen" @close="closeCommandPalette" />
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, defineAsyncComponent } from 'vue';
import { useThemeStore } from '~/stores/theme';

// Import sidebar components
const SidebarExplorer = defineAsyncComponent(() => import('~/components/SidebarExplorer.vue'));
const SidebarInfo = defineAsyncComponent(() => import('~/components/SidebarInfo.vue'));
const CommandPalette = defineAsyncComponent(() => import('~/components/CommandPalette.vue'));

// Theme store
const themeStore = useThemeStore();

// Initialize theme
onMounted(() => {
  themeStore.initialize();
});

// Sidebar state
const activeTab = ref('explorer'); // Default active tab
const isSidebarVisible = ref(true); // Sidebar visibility

// Command palette state
const isCommandPaletteOpen = ref(false);

// Set active tab
function setActiveTab(tab: string) {
  if (activeTab.value === tab) {
    // Toggle sidebar if clicking the active tab
    isSidebarVisible.value = !isSidebarVisible.value;
  } else {
    activeTab.value = tab;
    isSidebarVisible.value = true;
  }
}

// Toggle sidebar position (placeholder function)
function toggleSidebarPosition() {
  // This would be implemented to switch between left and right sidebar
  console.log('Toggle sidebar position');
}

// Toggle theme
function toggleTheme() {
  themeStore.toggleTheme();
}

// Command palette functions
function openCommandPalette() {
  isCommandPaletteOpen.value = true;
}

function closeCommandPalette() {
  isCommandPaletteOpen.value = false;
}

// Keyboard shortcut for command palette
function handleKeyDown(event: KeyboardEvent) {
  // Ctrl+P or Cmd+P
  if ((event.ctrlKey || event.metaKey) && event.key === 'p') {
    event.preventDefault();
    openCommandPalette();
  }
  
  // Escape to close command palette
  if (event.key === 'Escape' && isCommandPaletteOpen.value) {
    closeCommandPalette();
  }
}

// Add and remove event listeners
onMounted(() => {
  window.addEventListener('keydown', handleKeyDown);
});

onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeyDown);
});
</script>
