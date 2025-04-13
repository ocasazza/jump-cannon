import { defineStore } from 'pinia';

// Define the settings interface
export interface WorkspaceSettings {
  fontSize: number;
  fontFamily: string;
  showLineNumbers: boolean;
  // Add more settings as needed
}

// Define the settings store
export const useSettingsStore = defineStore('settings', {
  state: () => ({
    settings: {
      fontSize: 14,
      fontFamily: 'monospace',
      showLineNumbers: true,
    } as WorkspaceSettings,
    initialized: false
  }),
  
  getters: {
    getSettings: (state) => state.settings,
    getFontSize: (state) => state.settings.fontSize,
    getFontFamily: (state) => state.settings.fontFamily,
    getShowLineNumbers: (state) => state.settings.showLineNumbers,
  },
  
  actions: {
    // Initialize settings from localStorage
    initialize() {
      if (this.initialized) return;
      
      try {
        const savedSettings = localStorage.getItem('workspace-settings');
        if (savedSettings) {
          const parsedSettings = JSON.parse(savedSettings);
          this.settings = { ...this.settings, ...parsedSettings };
        }
        this.initialized = true;
      } catch (error) {
        console.error('Failed to load settings from localStorage:', error);
      }
    },
    
    // Save settings to localStorage
    saveSettings() {
      try {
        localStorage.setItem('workspace-settings', JSON.stringify(this.settings));
      } catch (error) {
        console.error('Failed to save settings to localStorage:', error);
      }
    },
    
    // Update a single setting
    updateSetting<K extends keyof WorkspaceSettings>(key: K, value: WorkspaceSettings[K]) {
      this.settings[key] = value;
      this.saveSettings();
    },
    
    // Update multiple settings at once
    updateSettings(newSettings: Partial<WorkspaceSettings>) {
      this.settings = { ...this.settings, ...newSettings };
      this.saveSettings();
    },
    
    // Reset settings to defaults
    resetSettings() {
      this.settings = {
        fontSize: 14,
        fontFamily: 'monospace',
        showLineNumbers: true,
      };
      this.saveSettings();
    }
  }
});
