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
          <FileTab
            :is-active="uiStore.sidebarActiveTab === 'file'"
            @click="uiStore.setSidebarTab('file')"
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
          
          <!-- File Tab -->
          <FileOperationsTab v-else-if="uiStore.sidebarActiveTab === 'file'" />
          
          <!-- Info Tab -->
          <SidebarInfo v-else-if="uiStore.sidebarActiveTab === 'info'" />
          
          <!-- Settings Tab -->
          <div v-else-if="uiStore.sidebarActiveTab === 'settings'" class="h-full overflow-y-auto">
            <div class="p-4">
              <h2 class="text-lg font-bold mb-3">Settings</h2>
              
              <!-- UI Settings -->
              <div class="mb-4">
                <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">UI Settings</h3>
                <div class="space-y-2">
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
              
              <!-- Workspace Settings -->
              <div class="mb-4">
                <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Workspace Settings</h3>
                <div class="space-y-4">
                  <!-- Font Size Setting -->
                  <div class="setting-item">
                    <label class="block text-sm font-medium mb-1">Font Size</label>
                    <div class="flex items-center">
                      <button 
                        @click="decreaseFontSize" 
                        class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded-l"
                        :disabled="settingsStore.settings.fontSize <= 8"
                      >
                        <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
                          <path fill-rule="evenodd" d="M3 10a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1z" clip-rule="evenodd" />
                        </svg>
                      </button>
                      <input 
                        v-model.number="fontSize" 
                        type="number" 
                        min="8" 
                        max="32" 
                        class="w-16 text-center py-1 bg-bg-secondary dark:bg-bg-dark-secondary border-y border-border dark:border-border-dark"
                        @change="updateFontSize"
                      />
                      <button 
                        @click="increaseFontSize" 
                        class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded-r"
                        :disabled="settingsStore.settings.fontSize >= 32"
                      >
                        <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
                          <path fill-rule="evenodd" d="M10 3a1 1 0 011 1v5h5a1 1 0 110 2h-5v5a1 1 0 11-2 0v-5H4a1 1 0 110-2h5V4a1 1 0 011-1z" clip-rule="evenodd" />
                        </svg>
                      </button>
                    </div>
                  </div>
                  
                  <!-- Font Family Setting -->
                  <div class="setting-item">
                    <label class="block text-sm font-medium mb-1">Font Family</label>
                    <select 
                      v-model="fontFamily" 
                      class="w-full p-2 bg-bg-secondary dark:bg-bg-dark-secondary border border-border dark:border-border-dark rounded"
                      @change="updateFontFamily"
                    >
                      <option value="monospace">Monospace</option>
                      <option value="sans-serif">Sans Serif</option>
                      <option value="serif">Serif</option>
                    </select>
                  </div>
                  
                  <!-- Line Numbers Setting -->
                  <div class="setting-item">
                    <div class="flex items-center">
                      <input 
                        id="show-line-numbers" 
                        v-model="showLineNumbers" 
                        type="checkbox"
                        class="mr-2"
                        @change="updateShowLineNumbers"
                      />
                      <label for="show-line-numbers" class="text-sm font-medium">Show Line Numbers</label>
                    </div>
                  </div>
                  
                  <!-- Actions -->
                  <div class="setting-item pt-4 border-t border-border dark:border-border-dark">
                    <button 
                      @click="resetSettings" 
                      class="px-3 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80 text-sm"
                    >
                      Reset to Defaults
                    </button>
                  </div>
                </div>
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
          
          <!-- File Tab -->
          <FileOperationsTab v-else-if="uiStore.sidebarActiveTab === 'file'" />
          
          <!-- Info Tab -->
          <SidebarInfo v-else-if="uiStore.sidebarActiveTab === 'info'" />
          
          <!-- Settings Tab -->
          <div v-else-if="uiStore.sidebarActiveTab === 'settings'" class="h-full overflow-y-auto">
            <div class="p-4">
              <h2 class="text-lg font-bold mb-3">Settings</h2>
              
              <!-- UI Settings -->
              <div class="mb-4">
                <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">UI Settings</h3>
                <div class="space-y-2">
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
              
              <!-- Workspace Settings -->
              <div class="mb-4">
                <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Workspace Settings</h3>
                <div class="space-y-4">
                  <!-- Font Size Setting -->
                  <div class="setting-item">
                    <label class="block text-sm font-medium mb-1">Font Size</label>
                    <div class="flex items-center">
                      <button 
                        @click="decreaseFontSize" 
                        class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded-l"
                        :disabled="settingsStore.settings.fontSize <= 8"
                      >
                        <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
                          <path fill-rule="evenodd" d="M3 10a1 1 0 011-1h12a1 1 0 110 2H4a1 1 0 01-1-1z" clip-rule="evenodd" />
                        </svg>
                      </button>
                      <input 
                        v-model.number="fontSize" 
                        type="number" 
                        min="8" 
                        max="32" 
                        class="w-16 text-center py-1 bg-bg-secondary dark:bg-bg-dark-secondary border-y border-border dark:border-border-dark"
                        @change="updateFontSize"
                      />
                      <button 
                        @click="increaseFontSize" 
                        class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded-r"
                        :disabled="settingsStore.settings.fontSize >= 32"
                      >
                        <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
                          <path fill-rule="evenodd" d="M10 3a1 1 0 011 1v5h5a1 1 0 110 2h-5v5a1 1 0 11-2 0v-5H4a1 1 0 110-2h5V4a1 1 0 011-1z" clip-rule="evenodd" />
                        </svg>
                      </button>
                    </div>
                  </div>
                  
                  <!-- Font Family Setting -->
                  <div class="setting-item">
                    <label class="block text-sm font-medium mb-1">Font Family</label>
                    <select 
                      v-model="fontFamily" 
                      class="w-full p-2 bg-bg-secondary dark:bg-bg-dark-secondary border border-border dark:border-border-dark rounded"
                      @change="updateFontFamily"
                    >
                      <option value="monospace">Monospace</option>
                      <option value="sans-serif">Sans Serif</option>
                      <option value="serif">Serif</option>
                    </select>
                  </div>
                  
                  <!-- Line Numbers Setting -->
                  <div class="setting-item">
                    <div class="flex items-center">
                      <input 
                        id="show-line-numbers-right" 
                        v-model="showLineNumbers" 
                        type="checkbox"
                        class="mr-2"
                        @change="updateShowLineNumbers"
                      />
                      <label for="show-line-numbers-right" class="text-sm font-medium">Show Line Numbers</label>
                    </div>
                  </div>
                  
                  <!-- Actions -->
                  <div class="setting-item pt-4 border-t border-border dark:border-border-dark">
                    <button 
                      @click="resetSettings" 
                      class="px-3 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80 text-sm"
                    >
                      Reset to Defaults
                    </button>
                  </div>
                </div>
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
          <FileTab
            :is-active="uiStore.sidebarActiveTab === 'file'"
            @click="uiStore.setSidebarTab('file')"
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
import { onMounted, onBeforeUnmount, computed, ref, watch } from 'vue';
import { useUIStore } from '~/stores/ui';
import { useThemeStore } from '~/stores/theme';
import { useSettingsStore } from '~/stores/settings';
import SearchTab from './SearchTab.vue';
import InfoTab from './InfoTab.vue';
import SettingsTab from './SettingsTab.vue';
import FileTab from './FileTab.vue';
import FileOperationsTab from './FileOperationsTab.vue';
import SidebarInfo from '~/components/SidebarInfo.vue';

// Stores
const uiStore = useUIStore();
const themeStore = useThemeStore();
const settingsStore = useSettingsStore();

// Settings state
const fontSize = ref(settingsStore.settings.fontSize);
const fontFamily = ref(settingsStore.settings.fontFamily);
const showLineNumbers = ref(settingsStore.settings.showLineNumbers);

// Watch for settings changes
watch(() => settingsStore.settings, (newSettings) => {
  fontSize.value = newSettings.fontSize;
  fontFamily.value = newSettings.fontFamily;
  showLineNumbers.value = newSettings.showLineNumbers;
}, { deep: true });

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

// Settings methods
function updateFontSize() {
  if (fontSize.value < 8) fontSize.value = 8;
  if (fontSize.value > 32) fontSize.value = 32;
  settingsStore.updateSetting('fontSize', fontSize.value);
}

function increaseFontSize() {
  if (fontSize.value < 32) {
    fontSize.value++;
    updateFontSize();
  }
}

function decreaseFontSize() {
  if (fontSize.value > 8) {
    fontSize.value--;
    updateFontSize();
  }
}

function updateFontFamily() {
  settingsStore.updateSetting('fontFamily', fontFamily.value);
}

function updateShowLineNumbers() {
  settingsStore.updateSetting('showLineNumbers', showLineNumbers.value);
}

function resetSettings() {
  settingsStore.resetSettings();
}

// Initialize UI store
onMounted(() => {
  uiStore.initialize();
  
  // Initialize settings
  fontSize.value = settingsStore.settings.fontSize;
  fontFamily.value = settingsStore.settings.fontFamily;
  showLineNumbers.value = settingsStore.settings.showLineNumbers;
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
