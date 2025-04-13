<template>
  <div class="action-list">
    <div v-if="instances.length === 0" class="text-center py-4 text-text-secondary dark:text-text-dark-secondary">
      No active actions
    </div>
    
    <div v-else class="space-y-2">
      <div v-for="instance in instances" :key="instance.id" class="action-instance">
        <ActionCard 
          :instance="instance" 
          @edit="editInstance(instance)" 
          @delete="deleteInstance(instance.id)" 
        />
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import { useActionsStore, type ActionInstance } from '~/stores/actions';
import ActionCard from '~/components/actions/ActionCard.vue';

// Props
const props = defineProps({
  filter: {
    type: String,
    default: 'all' // 'all', 'singleton', 'multi-instance'
  }
});

// Store
const actionsStore = useActionsStore();

// Computed
const instances = computed(() => {
  const allInstances = actionsStore.getActionInstances();
  
  if (props.filter === 'all') {
    return allInstances;
  }
  
  return allInstances.filter(instance => {
    const action = actionsStore.getActionById(instance.actionId);
    if (!action) return false;
    
    return props.filter === 'singleton' 
      ? action.type === 'singleton'
      : action.type === 'multi-instance';
  });
});

// Methods
function editInstance(instance: ActionInstance) {
  const action = actionsStore.getActionById(instance.actionId);
  if (!action || !action.parameters || action.parameters.length === 0) return;
  
  // Start configuring with existing params
  actionsStore.startConfiguring(instance.actionId, (params) => {
    // Update the existing instance with new params
    actionsStore.updateActionInstance(instance.id, params);
  });
}

function deleteInstance(instanceId: string) {
  actionsStore.removeActionInstance(instanceId);
}
</script>

<style scoped>
.action-list {
  @apply p-4;
}

.action-instance {
  @apply bg-bg-secondary dark:bg-bg-dark-secondary rounded-md shadow-sm;
}
</style>
