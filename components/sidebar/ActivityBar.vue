<template>
  <div class="flex h-full">
    <!-- Left-positioned sidebar -->
    <template v-if="isLeftSidebar">
      <!-- Activity Bar (left sidebar with icons) -->
      <div class="activity-bar">
        <div class="flex flex-col gap-4 pt-2">
          <SearchTab 
            :is-active="uiStore.sidebarActiveTab === 'search'" 
            @click="uiStore.setSidebarTab('search')" 
          />
          <InfoTab 
            :is-active="uiStore.sidebarActiveTab === 'info'" 
            @click="uiStore.setSidebarTab('info')" 
          />
          <SettingsTab 
            :is-active="uiStore.sidebarActiveTab === 'settings'" 
            @click="uiStore.setSidebarTab('settings')" 
          />
        </div>
      </div>

      <!-- Side Bar (explorer, search, etc.) -->
      <div 
        v-if="uiStore.sidebarVisible" 
        class="side-bar h-full flex flex-col relative"
        :style="{ width: `${uiStore.sidebarWidth}px` }"
      >
        <!-- Resize handle (right side) -->
        <div 
          class="absolute top-0 right-0 w-2 h-full cursor-ew-resize hover:bg-accent hover:opacity-50 z-10"
          @mousedown="(e) => startResize(false, e)"
        >
          <!-- Visual indicator for resize handle -->
          <div class="w-[1px] h-full bg-border dark:bg-border-dark mx-auto"></div>
        </div>
      
        <div class="flex-1 overflow-hidden">
          <!-- Search Tab (placeholder) -->
          <div v-if="uiStore.sidebarActiveTab === 'search'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Search</h2>
            <div class="mb-4">
              <input 
                type="text" 
                placeholder="Search..." 
                class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              />
            </div>
            <div class="text-sm text-text-tertiary dark:text-text-dark-tertiary">
              Type to search in files
            </div>
          </div>
          
          <!-- Info Tab -->
          <SidebarInfo v-else-if="uiStore.sidebarActiveTab === 'info'" />
          
          <!-- Settings Tab (placeholder) -->
          <div v-else-if="uiStore.sidebarActiveTab === 'settings'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Settings</h2>
            <div class="mb-4">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Theme</span>
                <button 
                  @click="toggleTheme" 
                  class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded text-sm"
                >
                  {{ themeStore.mode === 'light' ? 'Light' : 'Dark' }}
                </button>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Sidebar Position</span>
                <button 
                  @click="uiStore.toggleSidebarPosition" 
                  class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded text-sm"
                >
                  {{ uiStore.sidebarPosition === 'left' ? 'Left' : 'Right' }}
                </button>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Sidebar Width</span>
                <div class="text-sm">{{ uiStore.sidebarWidth }}px</div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </template>

    <!-- Right-positioned sidebar -->
    <template v-else>
      <!-- Side Bar (explorer, search, etc.) -->
      <div 
        v-if="uiStore.sidebarVisible" 
        class="side-bar h-full flex flex-col relative"
        :style="{ width: `${uiStore.sidebarWidth}px` }"
      >
        <!-- Resize handle (left side) -->
        <div 
          class="absolute top-0 left-0 w-2 h-full cursor-ew-resize hover:bg-accent hover:opacity-50 z-10"
          @mousedown="(e) => startResize(true, e)"
        >
          <!-- Visual indicator for resize handle -->
          <div class="w-[1px] h-full bg-border dark:bg-border-dark mx-auto"></div>
        </div>
        
        <div class="flex-1 overflow-hidden">
          <!-- Search Tab (placeholder) -->
          <div v-if="uiStore.sidebarActiveTab === 'search'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Search</h2>
            <div class="mb-4">
              <input 
                type="text" 
                placeholder="Search..." 
                class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              />
            </div>
            <div class="text-sm text-text-tertiary dark:text-text-dark-tertiary">
              Type to search in files
            </div>
          </div>
          
          <!-- Info Tab -->
          <SidebarInfo v-else-if="uiStore.sidebarActiveTab === 'info'" />
          
          <!-- Settings Tab (placeholder) -->
          <div v-else-if="uiStore.sidebarActiveTab === 'settings'" class="h-full overflow-y-auto p-4">
            <h2 class="text-lg font-bold mb-3">Settings</h2>
            <div class="mb-4">
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Theme</span>
                <button 
                  @click="toggleTheme" 
                  class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded text-sm"
                >
                  {{ themeStore.mode === 'light' ? 'Light' : 'Dark' }}
                </button>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Sidebar Position</span>
                <button 
                  @click="uiStore.toggleSidebarPosition" 
                  class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded text-sm"
                >
                  {{ uiStore.sidebarPosition === 'left' ? 'Left' : 'Right' }}
                </button>
              </div>
              <div class="flex items-center justify-between mb-2">
                <span class="text-sm">Sidebar Width</span>
                <div class="text-sm">{{ uiStore.sidebarWidth }}px</div>
              </div>
            </div>
          </div>
        </div>
      </div>
      
      <!-- Activity Bar (right sidebar with icons) -->
      <div class="activity-bar">
        <div class="flex flex-col gap-4 pt-2">
          <SearchTab 
            :is-active="uiStore.sidebarActiveTab === 'search'" 
            @click="uiStore.setSidebarTab('search')" 
          />
          <InfoTab 
            :is-active="uiStore.sidebarActiveTab === 'info'" 
            @click="uiStore.setSidebarTab('info')" 
          />
          <SettingsTab 
            :is-active="uiStore.sidebarActiveTab === 'settings'" 
            @click="uiStore.setSidebarTab('settings')" 
          />
        </div>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { onMounted, onBeforeUnmount, computed } from 'vue';
import { useUIStore } from '~/stores/ui';
import { useThemeStore } from '~/stores/theme';
import SearchTab from './SearchTab.vue';
import InfoTab from './InfoTab.vue';
import SettingsTab from './SettingsTab.vue';
import SidebarInfo from '~/components/SidebarInfo.vue';

// Stores
const uiStore = useUIStore();
const themeStore = useThemeStore();

// Computed properties
const isLeftSidebar = computed(() => uiStore.sidebarPosition === 'left');

// Toggle theme
function toggleTheme() {
  themeStore.toggleTheme();
}

// Resize functionality
let isResizing = false;
let startX = 0;
let startWidth = 0;
let isLeftResize = false;

// Unified resize function for both sides
function startResize(isLeft: boolean, event: MouseEvent) {
  isResizing = true;
  isLeftResize = isLeft;
  startX = event.clientX;
  startWidth = uiStore.sidebarWidth;
  
  // Add event listeners
  document.addEventListener('mousemove', handleResize);
  document.addEventListener('mouseup', stopResize);
  
  // Prevent text selection during resize
  document.body.style.userSelect = 'none';
  
  // Add visual feedback class to body during resize
  document.body.classList.add('resizing-sidebar');
}

function handleResize(event: MouseEvent) {
  if (!isResizing) return;
  
  // Calculate new width based on resize direction
  let deltaX = event.clientX - startX;
  
  // Invert delta for left-side resize
  if (isLeftResize) {
    deltaX = -deltaX;
  }
  
  const newWidth = startWidth + deltaX;
  
  // Apply width with requestAnimationFrame for smoother resizing
  requestAnimationFrame(() => {
    uiStore.setSidebarWidth(newWidth);
  });
  
  // Prevent default to avoid text selection
  event.preventDefault();
}

function stopResize() {
  isResizing = false;
  document.removeEventListener('mousemove', handleResize);
  document.removeEventListener('mouseup', stopResize);
  
  // Re-enable text selection
  document.body.style.userSelect = '';
  
  // Remove visual feedback class
  document.body.classList.remove('resizing-sidebar');
}

// Clean up event listeners
onBeforeUnmount(() => {
  document.removeEventListener('mousemove', handleResize);
  document.removeEventListener('mouseup', stopResize);
});

// Initialize UI store
onMounted(() => {
  uiStore.initialize();
});
</script>

<style scoped>
.activity-bar {
  width: 48px;
  background-color: var(--bg-secondary);
}

.side-bar {
  background-color: var(--bg-secondary);
}

/* Left sidebar styling */
:deep(.left-sidebar) .activity-bar {
  border-right: 1px solid var(--border);
}

:deep(.left-sidebar) .side-bar {
  border-right: 1px solid var(--border);
}

/* Right sidebar styling */
:deep(.right-sidebar) .activity-bar {
  border-left: 1px solid var(--border);
}

:deep(.right-sidebar) .side-bar {
  border-left: 1px solid var(--border);
}
</style>
