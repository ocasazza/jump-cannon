import { defineStore } from 'pinia';
import { ref } from 'vue';

export type SidebarTab = 'search' | 'info' | 'settings' | 'file';
export type SidebarPosition = 'left' | 'right';

export const useUIStore = defineStore('ui', () => {
  // Global loading state
  const isLoading = ref(false);
  
  // Sidebar state
  const sidebarActiveTab = ref<SidebarTab>('search');
  const sidebarVisible = ref(true);
  const sidebarWidth = ref(256); // Default width in pixels
  const sidebarPosition = ref<SidebarPosition>('left');
  
  // Command palette state
  const commandPaletteOpen = ref(false);
  
  // Initialize from localStorage
  function initialize() {
    if (typeof localStorage !== 'undefined') {
      try {
        const storedState = localStorage.getItem('uiState');
        if (storedState) {
          const parsedState = JSON.parse(storedState);
          
          // Apply stored values if they exist
          if (parsedState.sidebarActiveTab) sidebarActiveTab.value = parsedState.sidebarActiveTab;
          if (parsedState.sidebarVisible !== undefined) sidebarVisible.value = parsedState.sidebarVisible;
          if (parsedState.sidebarWidth) sidebarWidth.value = parsedState.sidebarWidth;
          if (parsedState.sidebarPosition) sidebarPosition.value = parsedState.sidebarPosition;
        }
      } catch (error) {
        console.error('Error loading UI state from localStorage:', error);
      }
    }
  }
  
  // Loading state actions
  function setLoading(loading: boolean) {
    isLoading.value = loading;
  }
  
  // Sidebar actions
  function setSidebarTab(tab: SidebarTab) {
    if (sidebarActiveTab.value === tab) {
      // Toggle sidebar if clicking the active tab
      sidebarVisible.value = !sidebarVisible.value;
    } else {
      sidebarActiveTab.value = tab;
      sidebarVisible.value = true;
    }
    
    // Save to localStorage
    saveUIState();
  }
  
  function toggleSidebarVisibility() {
    sidebarVisible.value = !sidebarVisible.value;
    saveUIState();
  }
  
  function setSidebarWidth(width: number) {
    // Set min/max constraints
    const minWidth = 200;
    const maxWidth = 500;
    sidebarWidth.value = Math.min(Math.max(width, minWidth), maxWidth);
    saveUIState();
  }
  
  function toggleSidebarPosition() {
    sidebarPosition.value = sidebarPosition.value === 'left' ? 'right' : 'left';
    saveUIState();
  }
  
  // Command palette actions
  function openCommandPalette() {
    commandPaletteOpen.value = true;
  }
  
  function closeCommandPalette() {
    commandPaletteOpen.value = false;
  }
  
  // Helper to save UI state to localStorage
  function saveUIState() {
    if (typeof localStorage !== 'undefined') {
      const uiState = {
        sidebarActiveTab: sidebarActiveTab.value,
        sidebarVisible: sidebarVisible.value,
        sidebarWidth: sidebarWidth.value,
        sidebarPosition: sidebarPosition.value,
      };
      localStorage.setItem('uiState', JSON.stringify(uiState));
    }
  }
  
  return {
    // State
    isLoading,
    sidebarActiveTab,
    sidebarVisible,
    sidebarWidth,
    sidebarPosition,
    commandPaletteOpen,
    
    // Actions
    initialize,
    setLoading,
    setSidebarTab,
    toggleSidebarVisibility,
    setSidebarWidth,
    toggleSidebarPosition,
    openCommandPalette,
    closeCommandPalette
  };
});
