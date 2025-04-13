<template>
  <div class="h-full overflow-y-auto">
    <div class="p-4">
      <h2 class="text-lg font-bold mb-3">Graph File Operations</h2>
      
      <!-- Drag and drop area -->
      <div 
        class="border-2 border-dashed border-border dark:border-border-dark rounded-lg p-6 mb-4 text-center"
        :class="{ 'border-accent': isDragging, 'bg-accent bg-opacity-5': isDragging }"
        @dragover.prevent="handleDragOver"
        @dragleave.prevent="handleDragLeave"
        @drop.prevent="handleFileDrop"
      >
        <div v-if="graphStore.hasGraph">
          <p class="mb-2">Current graph: <strong>{{ graphStore.getGraph?.name || 'Untitled Graph' }}</strong></p>
          <p class="text-sm mb-4">{{ graphStore.getNodeCount }} nodes, {{ graphStore.getEdgeCount }} edges</p>
          <button 
            @click="triggerFileInput" 
            class="px-3 py-1 bg-accent text-white rounded hover:bg-opacity-90 mb-2"
          >
            Replace Graph
          </button>
          <p class="text-xs text-text-tertiary dark:text-text-dark-tertiary">
            Or drag and drop a new file
          </p>
        </div>
        <div v-else>
          <p class="mb-2">Drag and drop a graph file here</p>
          <p class="text-sm text-text-tertiary dark:text-text-dark-tertiary mb-4">Supported formats: .json, .dot</p>
          <button 
            @click="triggerFileInput" 
            class="px-3 py-1 bg-accent text-white rounded hover:bg-opacity-90"
          >
            Select File
          </button>
        </div>
        <input 
          ref="fileInput"
          type="file" 
          accept=".json,.dot" 
          class="hidden"
          @change="handleFileSelect"
        />
      </div>
      
      <!-- Loading indicator -->
      <div v-if="graphStore.isLoading" class="bg-bg-tertiary dark:bg-bg-dark-tertiary p-3 rounded mb-4 flex items-center">
        <div class="animate-spin mr-2">
          <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <circle cx="12" cy="12" r="10" stroke-opacity="0.25"></circle>
            <path d="M12 2a10 10 0 0 1 10 10" stroke-opacity="0.75"></path>
          </svg>
        </div>
        <span>Loading graph...</span>
      </div>
      
      <!-- Error message -->
      <div v-if="graphStore.error" class="bg-red-100 dark:bg-red-900 text-red-800 dark:text-red-200 p-3 rounded mb-4">
        <div class="font-bold mb-1">Error</div>
        <div>{{ graphStore.error }}</div>
      </div>
      
      <!-- Export options (when graph is loaded) -->
      <div v-if="graphStore.hasGraph" class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Export Graph</h3>
        <div class="flex space-x-2">
          <button 
            @click="exportGraph('json')" 
            class="px-3 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-90"
          >
            Export as JSON
          </button>
          <button 
            @click="exportGraph('dot')" 
            class="px-3 py-1 bg-bg-tertiary dark:bg-bg-dark-tertiary rounded hover:bg-opacity-90"
          >
            Export as DOT
          </button>
        </div>
      </div>
      
      <!-- Graph information (when graph is loaded) -->
      <div v-if="graphStore.hasGraph" class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-text-secondary dark:text-text-dark-secondary">Graph Information</h3>
        <div class="bg-bg-tertiary dark:bg-bg-dark-tertiary p-3 rounded">
          <div class="grid grid-cols-2 gap-2 text-sm">
            <div class="font-medium">Name:</div>
            <div>{{ graphStore.getGraph?.name || 'Untitled' }}</div>
            
            <div class="font-medium">Nodes:</div>
            <div>{{ graphStore.getNodeCount }}</div>
            
            <div class="font-medium">Edges:</div>
            <div>{{ graphStore.getEdgeCount }}</div>
            
            <div class="font-medium">Description:</div>
            <div>{{ graphStore.getGraph?.description || 'No description' }}</div>
          </div>
        </div>
      </div>
      
      <!-- Help text (when no graph is loaded) -->
      <div v-if="!graphStore.hasGraph && !graphStore.isLoading" class="text-sm text-text-secondary dark:text-text-dark-secondary">
        <p class="mb-2">Upload a graph file to visualize and analyze it.</p>
        <p class="mb-2">Supported file formats:</p>
        <ul class="list-disc list-inside mb-2">
          <li><strong>JSON</strong> - Standard graph format with nodes and edges arrays</li>
          <li><strong>DOT</strong> - GraphViz DOT format for directed graphs</li>
        </ul>
        <p>Once loaded, you can apply filters and search operations from the command palette.</p>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue';
import { useGraphStore } from '~/stores/graph';

// Store
const graphStore = useGraphStore();

// Refs
const fileInput = ref<HTMLInputElement | null>(null);
const isDragging = ref(false);

// Methods
function triggerFileInput() {
  fileInput.value?.click();
}

function handleDragOver(event: DragEvent) {
  isDragging.value = true;
  
  // Check if the dragged items contain files
  if (event.dataTransfer?.items) {
    let hasValidFile = false;
    
    for (let i = 0; i < event.dataTransfer.items.length; i++) {
      const item = event.dataTransfer.items[i];
      
      if (item.kind === 'file') {
        const file = item.getAsFile();
        if (file) {
          const extension = file.name.split('.').pop()?.toLowerCase();
          if (extension === 'json' || extension === 'dot') {
            hasValidFile = true;
            break;
          }
        }
      }
    }
    
    if (!hasValidFile) {
      event.dataTransfer.dropEffect = 'none';
    } else {
      event.dataTransfer.dropEffect = 'copy';
    }
  }
}

function handleDragLeave() {
  isDragging.value = false;
}

function handleFileDrop(event: DragEvent) {
  isDragging.value = false;
  
  if (event.dataTransfer?.files && event.dataTransfer.files.length > 0) {
    const file = event.dataTransfer.files[0];
    loadFile(file);
  }
}

function handleFileSelect(event: Event) {
  const input = event.target as HTMLInputElement;
  
  if (input.files && input.files.length > 0) {
    const file = input.files[0];
    loadFile(file);
    
    // Reset the input so the same file can be selected again
    input.value = '';
  }
}

function loadFile(file: File) {
  const extension = file.name.split('.').pop()?.toLowerCase();
  
  if (extension !== 'json' && extension !== 'dot') {
    graphStore.error = `Unsupported file format: ${extension}. Please upload a .json or .dot file.`;
    return;
  }
  
  graphStore.loadGraphFromFile(file);
}

function exportGraph(format: 'json' | 'dot') {
  graphStore.exportGraph(format);
}
</script>
