<template>
  <div class="fixed inset-0 bg-black bg-opacity-50 z-50 flex items-start justify-center pt-[20vh]">
    <div class="bg-bg-secondary dark:bg-bg-dark-secondary rounded-md shadow-lg w-[500px] max-w-[90vw]">
      <div class="p-4 border-b border-border dark:border-border-dark">
        <h2 class="text-lg font-bold">Configure {{ action?.title }}</h2>
        <p class="text-sm text-text-secondary dark:text-text-dark-secondary">
          {{ action?.description }}
        </p>
      </div>
      
      <div class="p-4 max-h-[60vh] overflow-y-auto">
        <div v-if="!action || !action.parameters || action.parameters.length === 0" class="text-center py-4">
          <p class="text-text-secondary dark:text-text-dark-secondary">No parameters to configure</p>
        </div>
        
        <form v-else @submit.prevent="submitForm">
          <div v-for="param in action.parameters" :key="param.id" class="mb-4">
            <div class="mb-1">
              <label :for="param.id" class="block font-medium">
                {{ param.name }}
                <span v-if="param.required" class="text-error dark:text-error-dark">*</span>
              </label>
              <p class="text-xs text-text-secondary dark:text-text-dark-secondary">
                {{ param.description }}
              </p>
            </div>
            
            <!-- String parameter -->
            <input 
              v-if="param.type === 'string'" 
              :id="param.id"
              v-model="formValues[param.id]"
              type="text"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="param.required"
              :pattern="param.validation?.pattern"
            />
            
            <!-- Number parameter -->
            <input 
              v-else-if="param.type === 'number'" 
              :id="param.id"
              v-model.number="formValues[param.id]"
              type="number"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="param.required"
              :min="param.validation?.min"
              :max="param.validation?.max"
            />
            
            <!-- Boolean parameter -->
            <div v-else-if="param.type === 'boolean'" class="flex items-center">
              <input 
                :id="param.id"
                v-model="formValues[param.id]"
                type="checkbox"
                class="mr-2"
              />
              <label :for="param.id">Enable</label>
            </div>
            
            <!-- Select parameter -->
            <select 
              v-else-if="param.type === 'select'" 
              :id="param.id"
              v-model="formValues[param.id]"
              class="w-full p-2 bg-bg-tertiary dark:bg-bg-dark-tertiary border border-border dark:border-border-dark rounded"
              :required="param.required"
            >
              <option v-for="option in param.options" :key="option.value" :value="option.value">
                {{ option.label }}
              </option>
            </select>
            
            <!-- Multi-select parameter -->
            <div v-else-if="param.type === 'multiselect'" class="space-y-2">
              <div v-for="option in param.options" :key="option.value" class="flex items-center">
                <input 
                  :id="`${param.id}-${option.value}`"
                  type="checkbox"
                  :value="option.value"
                  v-model="multiSelectValues[param.id]"
                  class="mr-2"
                />
                <label :for="`${param.id}-${option.value}`">{{ option.label }}</label>
              </div>
            </div>
            
            <!-- Validation error message -->
            <p v-if="validationErrors[param.id]" class="text-xs text-error dark:text-error-dark mt-1">
              {{ validationErrors[param.id] }}
            </p>
          </div>
        </form>
      </div>
      
      <div class="p-4 border-t border-border dark:border-border-dark flex justify-end space-x-2">
        <button 
          @click="cancel" 
          class="px-4 py-2 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-80"
        >
          Cancel
        </button>
        <button 
          @click="submitForm" 
          class="px-4 py-2 bg-accent dark:bg-accent-dark text-white rounded hover:bg-opacity-80"
          :disabled="!isFormValid"
        >
          Apply
        </button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue';
import { useActionsStore, type Action, type ActionParameter } from '~/stores/actions';
import { useWorkspaceStore } from '~/stores/workspace';

// Props
const props = defineProps<{
  actionId: string
}>();

// Emits
const emit = defineEmits(['close', 'submit']);

// Stores
const actionsStore = useActionsStore();
const workspaceStore = useWorkspaceStore();

// Computed
const action = computed<Action | undefined>(() => 
  actionsStore.getActionById(props.actionId)
);

// Form state
const formValues = ref<Record<string, any>>({});
const multiSelectValues = ref<Record<string, any[]>>({});
const validationErrors = ref<Record<string, string>>({});

// Initialize form values with defaults or current values
onMounted(() => {
  if (action.value?.parameters) {
    // Check if this is a settings action
    const isSettingsAction = action.value.parentId === 'settings';
    
    action.value.parameters.forEach(param => {
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
});

// Watch multiSelectValues and update formValues
watch(multiSelectValues, (newValues) => {
  Object.entries(newValues).forEach(([paramId, values]) => {
    formValues.value[paramId] = values;
  });
}, { deep: true });

// Computed for form validity
const isFormValid = computed(() => {
  if (!action.value?.parameters) return true;
  
  return action.value.parameters.every(param => {
    if (param.required) {
      const value = formValues.value[param.id];
      if (value === undefined || value === null || value === '') {
        return false;
      }
      
      if (param.type === 'multiselect' && Array.isArray(value) && value.length === 0) {
        return false;
      }
    }
    
    // Check validation rules
    if (param.validation) {
      if (param.type === 'string' && param.validation.pattern) {
        const regex = new RegExp(param.validation.pattern);
        if (!regex.test(formValues.value[param.id])) {
          validationErrors.value[param.id] = `Invalid format`;
          return false;
        }
      }
      
      if (param.type === 'number') {
        const value = formValues.value[param.id];
        if (param.validation.min !== undefined && value < param.validation.min) {
          validationErrors.value[param.id] = `Minimum value is ${param.validation.min}`;
          return false;
        }
        if (param.validation.max !== undefined && value > param.validation.max) {
          validationErrors.value[param.id] = `Maximum value is ${param.validation.max}`;
          return false;
        }
      }
    }
    
    // Clear validation error if valid
    delete validationErrors.value[param.id];
    return true;
  });
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

// Methods
function submitForm() {
  if (!isFormValid.value) return;
  
  // Emit submit event with form values
  emit('submit', formValues.value);
  
  // Execute action with parameters
  actionsStore.finishConfiguring(formValues.value);
}

function cancel() {
  actionsStore.cancelConfiguring();
  emit('close');
}
</script>
