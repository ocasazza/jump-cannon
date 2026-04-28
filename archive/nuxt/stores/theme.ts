import { defineStore } from 'pinia';
import { ref, watch } from 'vue';

// Theme modes
export type ThemeMode = 'light' | 'dark';

// Define the theme store
export const useThemeStore = defineStore('theme', () => {
  // State
  const mode = ref<ThemeMode>('light');
  const systemPrefersDark = ref(false);
  const useSystemPreference = ref(true);
  
  // Initialize theme
  function initialize() {
    // Check if theme is stored in localStorage
    const storedTheme = localStorage.getItem('theme');
    const storedUseSystem = localStorage.getItem('useSystemPreference');
    
    // Set useSystemPreference based on stored value
    if (storedUseSystem !== null) {
      useSystemPreference.value = storedUseSystem === 'true';
    }
    
    // Check system preference
    checkSystemPreference();
    
    // Set theme based on stored value or system preference
    if (storedTheme && !useSystemPreference.value) {
      setTheme(storedTheme as ThemeMode);
    } else if (useSystemPreference.value) {
      setTheme(systemPrefersDark.value ? 'dark' : 'light');
    }
    
    // Watch for system preference changes
    if (typeof window !== 'undefined') {
      const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
      mediaQuery.addEventListener('change', checkSystemPreference);
    }
  }
  
  // Check system preference
  function checkSystemPreference() {
    if (typeof window !== 'undefined') {
      systemPrefersDark.value = window.matchMedia('(prefers-color-scheme: dark)').matches;
      
      if (useSystemPreference.value) {
        setTheme(systemPrefersDark.value ? 'dark' : 'light');
      }
    }
  }
  
  // Set theme
  function setTheme(newMode: ThemeMode) {
    mode.value = newMode;
    
    // Update document class
    if (typeof document !== 'undefined') {
      if (newMode === 'dark') {
        document.documentElement.classList.add('dark');
      } else {
        document.documentElement.classList.remove('dark');
      }
    }
    
    // Store theme in localStorage
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem('theme', newMode);
    }
  }
  
  // Toggle theme
  function toggleTheme() {
    const newMode = mode.value === 'light' ? 'dark' : 'light';
    setTheme(newMode);
    
    // Disable system preference when manually toggling
    useSystemPreference.value = false;
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem('useSystemPreference', 'false');
    }
  }
  
  // Use system theme
  function useSystemTheme() {
    useSystemPreference.value = true;
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem('useSystemPreference', 'true');
    }
    
    checkSystemPreference();
  }
  
  // Watch for changes to useSystemPreference
  watch(useSystemPreference, (newValue) => {
    if (newValue) {
      checkSystemPreference();
    }
  });
  
  return {
    mode,
    systemPrefersDark,
    useSystemPreference,
    initialize,
    setTheme,
    toggleTheme,
    useSystemTheme
  };
});
