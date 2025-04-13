<template>
  <div class="action-card">
    <div class="action-header">
      <div class="action-title">
        <h3 class="font-medium">{{ action?.title }}</h3>
        <span class="action-type">{{ action?.type }}</span>
      </div>
      <div class="action-controls">
        <button 
          v-if="hasParameters"
          @click="toggleEditMode" 
          class="edit-button"
          :title="isEditing ? 'Cancel editing' : 'Edit parameters'"
        >
          <svg v-if="!isEditing" xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
            <path d="M13.586 3.586a2 2 0 112.828 2.828l-.793.793-2.828-2.828.793-.793zM11.379 5.793L3 14.172V17h2.828l8.38-8.379-2.83-2.828z" />
          </svg>
          <svg v-else xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
            <path fill-rule="evenodd" d="M4.293 4.293a1 1 0 011.414 0L10 8.586l4.293-4.293a1 1 0 111.414 1.414L11.414 10l4.293 4.293a1 1 0 01-1.414 1.414L10 11.414l-4.293 4.293a1 1 0 01-1.414-1.414L8.586 10 4.293 5.707a1 1 0 010-1.414z" clip-rule="evenodd" />
          </svg>
        </button>
        <button 
          @click="$emit('delete', instance.id)" 
          class="delete-button"
          title="Delete action"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-4 w-4" viewBox="0 0 20 20" fill="currentColor">
            <path fill-rule="evenodd" d="M9 2a1 1 0 00-.894.553L7.382 4H4a1 1 0 000 2v10a2 2 0 002 2h8a2 2 0 002-2V6a1 1 0 100-2h-3.382l-.724-1.447A1 1 0 0011 2H9zM7 8a1 1 0 012 0v6a1 1 0 11-2 0V8zm5-1a1 1 0 00-1 1v6a1 1 0 102 0V8a1 1 0 00-1-1z" clip-rule="evenodd" />
          </svg>
        </button>
      </div>
    </div>
    
    <div class="action-description">
      {{ action?.description }}
    </div>
    
    <div v-if="hasParameters" class="action-params">
      <div class="params-header">Parameters</div>
      <div class="params-list">
        <div v-for="param in actionParameters" :key="param.id" class="param-item">
          <span class="param-name">{{ param.name }}:</span>
          
          <!-- Display editable fields when in edit mode -->
          <div v-if="isEditing" class="param-value-container">
            <!-- String parameter -->
            <input 
              v-if="param.type === 'string'" 
              v-model="editableParams[param.id]"
              type="text"
              class="param-input"
            />
            
            <!-- Number parameter -->
            <input 
              v-else-if="param.type === 'number'" 
              v-model.number="editableParams[param.id]"
              type="number"
              class="param-input"
              :min="param.validation?.min"
              :max="param.validation?.max"
            />
            
            <!-- Boolean parameter -->
            <div v-else-if="param.type === 'boolean'" class="flex items-center">
              <input 
                :id="`param-${param.id}`"
                v-model="editableParams[param.id]"
                type="checkbox"
                class="param-checkbox"
              />
              <label :for="`param-${param.id}`" class="ml-1 text-sm">{{ editableParams[param.id] ? 'Yes' : 'No' }}</label>
            </div>
            
            <!-- Select parameter -->
            <select 
              v-else-if="param.type === 'select'" 
              v-model="editableParams[param.id]"
              class="param-select"
            >
              <option v-for="option in param.options" :key="option.value" :value="option.value">
                {{ option.label }}
              </option>
            </select>
            
            <!-- Multi-select parameter -->
            <div v-else-if="param.type === 'multiselect'" class="space-y-1">
              <div v-for="option in param.options" :key="option.value" class="flex items-center">
                <input 
                  :id="`${param.id}-${option.value}`"
                  type="checkbox"
                  :value="option.value"
                  v-model="multiSelectValues[param.id]"
                  class="mr-1"
                />
                <label :for="`${param.id}-${option.value}`" class="text-xs">{{ option.label }}</label>
              </div>
            </div>
            
            <!-- Default display for unknown types -->
            <span v-else class="param-value">{{ formatParamValue(instance.params?.[param.id]) }}</span>
          </div>
          
          <!-- Display formatted value when not editing -->
          <span v-else class="param-value">{{ formatParamValue(instance.params?.[param.id]) }}</span>
        </div>
      </div>
      
      <!-- Action buttons for edit mode -->
      <div v-if="isEditing" class="param-actions">
        <button 
          @click="cancelEdit" 
          class="cancel-button"
        >
          Cancel
        </button>
        <button 
          @click="saveParameters" 
          class="save-button"
          :disabled="!hasChanges"
        >
          Save Changes
        </button>
      </div>
    </div>
    
    <div class="action-state">
      <div class="state-header">State</div>
      <pre class="state-content">{{ JSON.stringify(instance.state, null, 2) }}</pre>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, onMounted, watch } from 'vue';
import { useActionsStore, type ActionInstance, type ActionParameter } from '~/stores/actions';

// Props
const props = defineProps<{
  instance: ActionInstance
}>();

// Emits
defineEmits(['edit', 'delete']);

// Store
const actionsStore = useActionsStore();

// State
const isEditing = ref(false);
const editableParams = ref<Record<string, any>>({});
const originalParams = ref<Record<string, any>>({});
const multiSelectValues = ref<Record<string, any[]>>({});

// Computed
const action = computed(() => 
  actionsStore.getActionById(props.instance.actionId)
);

const actionParameters = computed(() => 
  action.value?.parameters || []
);

const hasParameters = computed(() => 
  actionParameters.value.length > 0 && 
  props.instance.params && 
  Object.keys(props.instance.params).length > 0
);

const hasChanges = computed(() => {
  return Object.keys(editableParams.value).some(key => {
    // Handle special cases like arrays
    if (Array.isArray(editableParams.value[key]) && Array.isArray(originalParams.value[key])) {
      if (editableParams.value[key].length !== originalParams.value[key].length) return true;
      return editableParams.value[key].some((val, idx) => val !== originalParams.value[key][idx]);
    }
    return editableParams.value[key] !== originalParams.value[key];
  });
});

// Initialize editable params
onMounted(() => {
  initializeParams();
});

// Watch for changes to multiSelectValues and update editableParams
watch(multiSelectValues, (newValues) => {
  Object.entries(newValues).forEach(([paramId, values]) => {
    editableParams.value[paramId] = values;
  });
}, { deep: true });

// Methods
function initializeParams() {
  if (props.instance.params) {
    editableParams.value = JSON.parse(JSON.stringify(props.instance.params));
    originalParams.value = JSON.parse(JSON.stringify(props.instance.params));
    
    // Initialize multiselect values
    actionParameters.value.forEach(param => {
      if (param.type === 'multiselect') {
        multiSelectValues.value[param.id] = props.instance.params?.[param.id] || [];
      }
    });
  }
}

function getParameterName(paramId: string): string {
  if (!action.value?.parameters) return paramId;
  
  const param = action.value.parameters.find(p => p.id === paramId);
  return param ? param.name : paramId;
}

function formatParamValue(value: any): string {
  if (value === undefined || value === null) {
    return 'Not set';
  }
  
  if (typeof value === 'boolean') {
    return value ? 'Yes' : 'No';
  }
  
  if (Array.isArray(value)) {
    return value.join(', ');
  }
  
  if (typeof value === 'object') {
    return JSON.stringify(value);
  }
  
  return String(value);
}

function toggleEditMode() {
  if (isEditing.value) {
    cancelEdit();
  } else {
    isEditing.value = true;
    initializeParams();
  }
}

function cancelEdit() {
  isEditing.value = false;
  initializeParams();
}

async function saveParameters() {
  try {
    // Update the existing action instance with new parameters
    await actionsStore.updateActionInstance(props.instance.id, editableParams.value);
    isEditing.value = false;
  } catch (error) {
    console.error('Failed to update action instance:', error);
  }
}
</script>

<style scoped>
.action-card {
  @apply p-4 space-y-3;
}

.action-header {
  @apply flex justify-between items-start;
}

.action-title {
  @apply flex items-center space-x-2;
}

.action-type {
  @apply text-xs px-2 py-0.5 rounded-full bg-bg-tertiary dark:bg-bg-dark-tertiary text-text-secondary dark:text-text-dark-secondary;
}

.action-controls {
  @apply flex space-x-2;
}

.edit-button, .delete-button {
  @apply p-1 rounded-full hover:bg-bg-tertiary dark:hover:bg-bg-dark-tertiary text-text-secondary dark:text-text-dark-secondary hover:text-text-primary dark:hover:text-text-dark-primary transition-colors;
}

.delete-button {
  @apply hover:text-error dark:hover:text-error-dark;
}

.action-description {
  @apply text-sm text-text-secondary dark:text-text-dark-secondary;
}

.action-params, .action-state {
  @apply text-sm border border-border dark:border-border-dark rounded-md overflow-hidden;
}

.params-header, .state-header {
  @apply px-3 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary font-medium border-b border-border dark:border-border-dark;
}

.params-list {
  @apply p-3 space-y-1;
}

.param-item {
  @apply flex;
}

.param-name {
  @apply font-medium mr-2;
}

.param-value {
  @apply text-text-secondary dark:text-text-dark-secondary;
}

.state-content {
  @apply p-3 text-xs overflow-x-auto bg-bg-tertiary dark:bg-bg-dark-tertiary bg-opacity-50 dark:bg-opacity-50 font-mono;
}
</style>
