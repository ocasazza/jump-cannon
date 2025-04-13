<template>
  <div class="fixed inset-0 bg-black bg-opacity-50 z-50 flex items-start justify-center pt-[20vh]" @click.self="close">
    <div class="command-palette">
      <!-- Breadcrumb navigation for current path in the tree -->
      <div v-if="currentPath.length > 0" class="command-breadcrumb">
        <span 
          v-for="(item, index) in currentPath" 
          :key="index"
          @click="navigateToLevel(index)"
          class="breadcrumb-item"
        >
          {{ item.title }}
          <span v-if="index < currentPath.length - 1" class="mx-1">/</span>
        </span>
      </div>
      
      <!-- Action search mode -->
      <template v-if="!configuringActionId">
        <div class="p-2">
          <input 
            ref="inputRef"
            v-model="searchQuery" 
            type="text" 
            placeholder="Type a command or search..." 
            class="command-input"
            @keydown.down.prevent="selectNextItem"
            @keydown.up.prevent="selectPrevItem"
            @keydown.enter="executeSelected"
            @keydown.esc="close"
            @keydown.tab.prevent="completeWithSelected"
          />
        </div>
        
        <div class="command-results">
          <!-- Categories (only at root level) -->
          <div v-if="currentPath.length === 0 && !searchQuery && categories.length > 0" class="category-section">
            <div class="category-header">Categories</div>
            <div 
              v-for="category in categories" 
              :key="category"
              class="category-item"
              @click="selectCategory(category)"
            >
              {{ category }}
            </div>
          </div>
          
          <div v-if="filteredActions.length === 0" class="p-4 text-text-tertiary dark:text-text-dark-tertiary text-center">
            No matching actions found
          </div>
          <div v-else>
            <div 
              v-for="(action, index) in filteredActions" 
              :key="action.id"
              :class="[
                'command-item',
                selectedIndex === index ? 'active' : ''
              ]"
              @click="selectAction(action)"
              @mouseenter="selectedIndex = index"
            >
              <div class="flex-1">
                <div class="font-medium" v-html="highlightMatches(action.title)"></div>
                <div class="text-xs text-text-secondary dark:text-text-dark-secondary" v-html="highlightMatches(action.description)"></div>
              </div>
              <div class="flex items-center space-x-2">
                <div v-if="action.childrenIds?.length" class="has-children-indicator">
                  <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
                    <path fill-rule="evenodd" d="M7.293 14.707a1 1 0 010-1.414L10.586 10 7.293 6.707a1 1 0 011.414-1.414l4 4a1 1 0 010 1.414l-4 4a1 1 0 01-1.414 0z" clip-rule="evenodd" />
                  </svg>
                </div>
                <div class="text-xs px-1.5 py-0.5 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded">
                  {{ action.type === ActionType.SINGLETON ? 'Singleton' : 'Multi-instance' }}
                </div>
              </div>
            </div>
          </div>
        </div>
      </template>
      
      <!-- Parameter configuration mode -->
      <template v-else>
        <div class="p-4 border-b border-border dark:border-border-dark">
          <h2 class="text-lg font-bold">Configure {{ configuringAction?.title }}</h2>
          <p class="text-sm text-text-secondary dark:text-text-dark-secondary">
            {{ configuringAction?.description }}
          </p>
          
          <!-- Parameter navigation -->
          <div v-if="configuringAction?.parameters && configuringAction.parameters.length > 1" class="mt-2 flex items-center text-xs">
            <div class="flex-1">
              <span class="text-text-secondary dark:text-text-dark-secondary">
                Parameter {{ currentParamIndex + 1 }} of {{ configuringAction.parameters.length }}
              </span>
            </div>
            <div class="flex space-x-2">
              <button 
                @click="prevParameter" 
                class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80"
                :disabled="currentParamIndex === 0"
              >
                Previous
              </button>
              <button 
                @click="nextParameter" 
                class="px-2 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80"
                :disabled="currentParamIndex === configuringAction.parameters.length - 1"
              >
                Next
              </button>
            </div>
          </div>
        </div>
        
        <div class="p-4">
          <!-- Current parameter -->
          <div v-if="currentParam" class="mb-4">
            <div class="mb-1">
              <label :for="currentParam.id" class="block font-medium">
                {{ currentParam.name }}
                <span v-if="currentParam.required" class="text-error dark:text-error-dark">*</span>
              </label>
              <p class="text-xs text-text-secondary dark:text-text-dark-secondary">
                {{ currentParam.description }}
              </p>
            </div>
            
            <!-- String parameter -->
            <input 
              v-if="currentParam.type === 'string'" 
              :id="currentParam.id"
              v-model="formValues[currentParam.id]"
              type="text"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="currentParam.required"
              :pattern="currentParam.validation?.pattern"
              @keydown.enter="handleEnterKey"
            />
            
            <!-- Number parameter -->
            <input 
              v-else-if="currentParam.type === 'number'" 
              :id="currentParam.id"
              v-model.number="formValues[currentParam.id]"
              type="number"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="currentParam.required"
              :min="currentParam.validation?.min"
              :max="currentParam.validation?.max"
              @keydown.enter="handleEnterKey"
            />
            
            <!-- Boolean parameter -->
            <div v-else-if="currentParam.type === 'boolean'" class="flex items-center">
              <input 
                :id="currentParam.id"
                v-model="formValues[currentParam.id]"
                type="checkbox"
                class="mr-2"
              />
              <label :for="currentParam.id">Enable</label>
            </div>
            
            <!-- Select parameter -->
            <select 
              v-else-if="currentParam.type === 'select'" 
              :id="currentParam.id"
              v-model="formValues[currentParam.id]"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="currentParam.required"
            >
              <option v-for="option in currentParam.options" :key="option.value" :value="option.value">
                {{ option.label }}
              </option>
            </select>
            
            <!-- Multi-select parameter -->
            <div v-else-if="currentParam.type === 'multiselect'" class="space-y-2">
              <div v-for="option in currentParam.options" :key="option.value" class="flex items-center">
                <input 
                  :id="`${currentParam.id}-${option.value}`"
                  type="checkbox"
                  :value="option.value"
                  v-model="multiSelectValues[currentParam.id]"
                  class="mr-2"
                />
                <label :for="`${currentParam.id}-${option.value}`">{{ option.label }}</label>
              </div>
            </div>
            
            <!-- Validation error message -->
            <p v-if="validationErrors[currentParam.id]" class="text-xs text-error dark:text-error-dark mt-1">
              {{ validationErrors[currentParam.id] }}
            </p>
          </div>
          
          <!-- Action buttons -->
          <div class="flex justify-end space-x-2 mt-4">
            <button 
              @click="cancelConfiguring" 
              class="px-4 py-2 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80"
            >
              Cancel
            </button>
            <button 
              v-if="isLastParameter"
              @click="submitForm" 
              class="px-4 py-2 bg-accent dark:bg-accent-dark text-white rounded hover:bg-opacity-80"
              :disabled="!isCurrentParamValid"
            >
              Apply
            </button>
            <button 
              v-else
              @click="nextParameter" 
              class="px-4 py-2 bg-accent dark:bg-accent-dark text-white rounded hover:bg-opacity-80"
              :disabled="!isCurrentParamValid"
            >
              Next
            </button>
          </div>
        </div>
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, nextTick, watch } from 'vue';
import Fuse from 'fuse.js';
import { useActionsStore, ActionType, type Action, type ActionParameter } from '~/stores/actions';
import { useWorkspaceStore } from '~/stores/workspace';

// Props and emits
const emit = defineEmits(['close']);

// Refs
const inputRef = ref<HTMLInputElement | null>(null);
const searchQuery = ref('');
const selectedIndex = ref(0);
const currentPath = ref<any[]>([]);
const selectedCategory = ref<string | null>(null);
const configuringActionId = ref<string | null>(null);
const currentParamIndex = ref(0);
const formValues = ref<Record<string, any>>({});
const multiSelectValues = ref<Record<string, any[]>>({});
const validationErrors = ref<Record<string, string>>({});

// Get actions store
const actionsStore = useActionsStore();

// Computed properties for parameter configuration
const configuringAction = computed<Action | undefined>(() => 
  configuringActionId.value ? actionsStore.getActionById(configuringActionId.value) : undefined
);

const currentParam = computed<ActionParameter | undefined>(() => {
  if (!configuringAction.value?.parameters) return undefined;
  if (currentParamIndex.value >= configuringAction.value.parameters.length) return undefined;
  return configuringAction.value.parameters[currentParamIndex.value];
});

const isLastParameter = computed(() => {
  if (!configuringAction.value?.parameters) return true;
  return currentParamIndex.value === configuringAction.value.parameters.length - 1;
});

const isCurrentParamValid = computed(() => {
  if (!currentParam.value) return true;
  
  // Check if required and has value
  if (currentParam.value.required) {
    const value = formValues.value[currentParam.value.id];
    if (value === undefined || value === null || value === '') {
      validationErrors.value[currentParam.value.id] = 'This field is required';
      return false;
    }
    
    if (currentParam.value.type === 'multiselect' && Array.isArray(value) && value.length === 0) {
      validationErrors.value[currentParam.value.id] = 'Please select at least one option';
      return false;
    }
  }
  
  // Check validation rules
  if (currentParam.value.validation) {
    if (currentParam.value.type === 'string' && currentParam.value.validation.pattern) {
      const regex = new RegExp(currentParam.value.validation.pattern);
      if (!regex.test(formValues.value[currentParam.value.id])) {
        validationErrors.value[currentParam.value.id] = `Invalid format`;
        return false;
      }
    }
    
    if (currentParam.value.type === 'number') {
      const value = formValues.value[currentParam.value.id];
      if (currentParam.value.validation.min !== undefined && value < currentParam.value.validation.min) {
        validationErrors.value[currentParam.value.id] = `Minimum value is ${currentParam.value.validation.min}`;
        return false;
      }
      if (currentParam.value.validation.max !== undefined && value > currentParam.value.validation.max) {
        validationErrors.value[currentParam.value.id] = `Maximum value is ${currentParam.value.validation.max}`;
        return false;
      }
    }
  }
  
  // Clear validation error if valid
  delete validationErrors.value[currentParam.value.id];
  return true;
});

// Configure Fuse.js options
const fuseOptions = {
  keys: ['title', 'description', 'keywords'],
  threshold: 0.4,        // Lower threshold = stricter matching
  distance: 100,         // How far to search for matches
  includeScore: true,    // Include match score for sorting
  includeMatches: true,  // Include match details for highlighting
  ignoreLocation: true,  // Search the entire string
  useExtendedSearch: true // Enable extended search features
};

// Create Fuse instance with all actions
const fuse = computed(() => new Fuse(getAvailableActions(), fuseOptions));

// Get available actions based on current navigation state
function getAvailableActions() {
  // If at root level
  if (currentPath.value.length === 0) {
    // If category is selected, show actions in that category
    if (selectedCategory.value) {
      return actionsStore.getActionsByCategory(selectedCategory.value);
    }
    // Otherwise show root actions
    return actionsStore.getRootActions;
  }
  
  // If navigated into a parent action, show its children
  const currentParentId = currentPath.value[currentPath.value.length - 1].id;
  return actionsStore.getChildActions(currentParentId);
}

// Available categories
const categories = computed(() => actionsStore.getCategories);

// Computed properties
const filteredActions = computed(() => {
  if (!searchQuery.value) return getAvailableActions();
  
  // If searching, use fuzzy search
  const results = fuse.value.search(searchQuery.value);
  
  // Return the matched items
  return results.map(result => result.item);
});

// Reset selected index when filtered actions change
watch(filteredActions, () => {
  selectedIndex.value = 0;
});

// Methods
function selectNextItem() {
  if (filteredActions.value.length === 0) return;
  selectedIndex.value = (selectedIndex.value + 1) % filteredActions.value.length;
}

function selectPrevItem() {
  if (filteredActions.value.length === 0) return;
  selectedIndex.value = (selectedIndex.value - 1 + filteredActions.value.length) % filteredActions.value.length;
}

function completeWithSelected() {
  if (filteredActions.value.length === 0) return;
  const selectedAction = filteredActions.value[selectedIndex.value];
  searchQuery.value = selectedAction.title;
}

async function executeSelected() {
  if (filteredActions.value.length === 0) return;
  const selectedAction = filteredActions.value[selectedIndex.value];
  selectAction(selectedAction);
}

function selectCategory(category: string) {
  selectedCategory.value = category;
}

function selectAction(action: any) {
  // If action has children, navigate into it
  if (action.childrenIds?.length) {
    currentPath.value.push(action);
    searchQuery.value = '';
  } else {
    // Check if this is a setting action with a single parameter
    if (action.parameters?.length === 1 && action.parentId === 'settings') {
      // Use quick parameter input for settings
      quickParameterInput(action);
    } else {
      // Otherwise start the normal execution flow
      startActionExecution(action.id);
    }
  }
}

// Quick parameter input for settings with a single parameter
async function quickParameterInput(action: any) {
  if (!action.parameters || action.parameters.length !== 1) {
    return startActionExecution(action.id);
  }
  
  const param = action.parameters[0];
  let value: any;
  
  // Always show the parameter form for settings actions
  startActionExecution(action.id);
}

function navigateToLevel(level: number) {
  currentPath.value = currentPath.value.slice(0, level + 1);
}

async function startActionExecution(actionId: string) {
  const action = actionsStore.getActionById(actionId);
  if (!action) return;
  
  // Check if action has parameters
  if (action.parameters && action.parameters.length > 0) {
    // Show parameter configuration form
    configuringActionId.value = actionId;
    
    // Start configuring in the store
    actionsStore.startConfiguring(actionId, async (params) => {
      try {
        await actionsStore.executeAction(actionId, params);
        close();
      } catch (error) {
        console.error('Failed to execute action:', error);
      }
    });
  } else {
    // Execute action without parameters
    try {
      await actionsStore.executeAction(actionId);
      close();
    } catch (error) {
      console.error('Failed to execute action:', error);
    }
  }
}

function cancelConfiguring() {
  configuringActionId.value = null;
  actionsStore.cancelConfiguring();
}

function finishConfiguring(params: Record<string, any>) {
  configuringActionId.value = null;
}

function close() {
  emit('close');
}

// Highlight matched characters in search results
function highlightMatches(text: string): string {
  if (!searchQuery.value) return text;
  
  try {
    // Find the result for this text
    const results = fuse.value.search(searchQuery.value);
    const result = results.find(r => 
      r.item.title === text || 
      r.item.description === text
    );
    
    if (!result || !result.matches) return text;
    
    // Get matches for this specific field
    const matches = result.matches.filter(m => m.value === text);
    if (!matches.length) return text;
    
    // Apply highlighting
    let highlighted = text;
    let offset = 0;
    
    // For each matching index range
    matches[0].indices.forEach(([start, end]) => {
      const before = highlighted.substring(0, start + offset);
      const match = highlighted.substring(start + offset, end + 1 + offset);
      const after = highlighted.substring(end + 1 + offset);
      
      highlighted = `${before}<span class="highlight">${match}</span>${after}`;
      offset += '<span class="highlight">'.length + '</span>'.length;
    });
    
    return highlighted;
  } catch (error) {
    console.error('Error highlighting matches:', error);
    return text;
  }
}

// Parameter navigation methods
function prevParameter() {
  if (currentParamIndex.value > 0) {
    currentParamIndex.value--;
  }
}

function nextParameter() {
  if (currentParam.value && isCurrentParamValid.value) {
    if (currentParamIndex.value < (configuringAction.value?.parameters?.length || 0) - 1) {
      currentParamIndex.value++;
    }
  }
}

function handleEnterKey() {
  if (isCurrentParamValid.value) {
    if (isLastParameter.value) {
      submitForm();
    } else {
      nextParameter();
    }
  }
}

function submitForm() {
  if (!configuringAction.value) return;
  
  // Check if all parameters are valid
  let allValid = true;
  configuringAction.value.parameters?.forEach((param, index) => {
    // Temporarily set currentParamIndex to check each parameter
    const originalIndex = currentParamIndex.value;
    currentParamIndex.value = index;
    
    if (!isCurrentParamValid.value) {
      allValid = false;
    }
    
    // Restore original index
    currentParamIndex.value = originalIndex;
  });
  
  if (!allValid) return;
  
  // Process multiselect values
  Object.entries(multiSelectValues.value).forEach(([paramId, values]) => {
    formValues.value[paramId] = values;
  });
  
  // Execute action with parameters
  actionsStore.finishConfiguring(formValues.value);
  configuringActionId.value = null;
}

// Initialize parameter form when configuringActionId changes
watch(configuringActionId, (newId) => {
  if (newId) {
    // Reset parameter index
    currentParamIndex.value = 0;
    
    // Clear previous values
    formValues.value = {};
    multiSelectValues.value = {};
    validationErrors.value = {};
    
    // Initialize form values with defaults
    if (configuringAction.value?.parameters) {
      const action = configuringAction.value;
      const workspaceStore = useWorkspaceStore();
      
      // For settings actions, use current values from workspace store
      const isSettingsAction = action.parentId === 'settings';
      
      action.parameters?.forEach(param => {
        if (param.type === 'multiselect') {
          // For multiselect, initialize with current value or default
          if (isSettingsAction && param.id in workspaceStore.settings) {
            const currentValue = workspaceStore.settings[param.id as keyof typeof workspaceStore.settings];
            if (Array.isArray(currentValue)) {
              multiSelectValues.value[param.id] = currentValue;
              formValues.value[param.id] = currentValue;
            } else {
              multiSelectValues.value[param.id] = param.default || [];
              formValues.value[param.id] = param.default || [];
            }
          } else {
            multiSelectValues.value[param.id] = param.default || [];
            formValues.value[param.id] = param.default || [];
          }
        } else {
          // For other types, use current value from store if it's a settings action
          if (isSettingsAction && param.id in workspaceStore.settings) {
            const currentValue = workspaceStore.settings[param.id as keyof typeof workspaceStore.settings];
            formValues.value[param.id] = currentValue;
          } else {
            formValues.value[param.id] = param.default !== undefined ? param.default : getDefaultValueForType(param.type);
          }
        }
      });
    }
  }
});

// Helper function to get default value based on parameter type
function getDefaultValueForType(type: string): any {
  switch (type) {
    case 'string': return '';
    case 'number': return 0;
    case 'boolean': return false;
    case 'select': return '';
    case 'multiselect': return [];
    default: return null;
  }
}

// Focus input on mount
onMounted(async () => {
  await nextTick();
  inputRef.value?.focus();
});
</script>

<style scoped>
.command-palette {
  @apply bg-bg-primary dark:bg-bg-dark-primary rounded-md shadow-lg w-[600px] max-w-[90vw] overflow-hidden;
}

.command-input {
  @apply w-full p-2 bg-bg-secondary dark:bg-bg-dark-secondary border border-border dark:border-border-dark rounded focus:outline-none focus:ring-1 focus:ring-accent;
}

.command-results {
  @apply max-h-[60vh] overflow-y-auto;
}

.command-item {
  @apply flex items-start p-3 hover:bg-bg-secondary dark:hover:bg-bg-dark-secondary cursor-pointer;
}

.command-item.active {
  @apply bg-bg-secondary dark:bg-bg-dark-secondary;
}

.command-breadcrumb {
  @apply flex items-center p-2 text-sm bg-bg-secondary dark:bg-bg-dark-secondary border-b border-border dark:border-border-dark;
}

.breadcrumb-item {
  @apply cursor-pointer hover:text-accent;
}

.category-section {
  @apply p-2 border-b border-border dark:border-border-dark;
}

.category-header {
  @apply text-xs font-medium text-text-secondary dark:text-text-dark-secondary uppercase mb-1 px-1;
}

.category-item {
  @apply p-2 rounded hover:bg-bg-secondary dark:hover:bg-bg-dark-secondary cursor-pointer;
}

.has-children-indicator {
  @apply text-text-secondary dark:text-text-dark-secondary;
}

:deep(.highlight) {
  @apply text-accent font-semibold;
}
</style>
