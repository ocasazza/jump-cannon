import { defineStore } from 'pinia';
import { useNuxtApp } from 'nuxt/app';
import { useActionsStore } from './actions';
import type { LayoutManager } from '~/wasm/rust-graph-layouts/pkg/rust-graph-layouts';

export interface Node {
  id: string;
  label: string;
  position?: [number, number];
  metadata: Record<string, any>;
  type?: string;
  x: number;
  y: number;
  [key: string]: any;  // Allow additional properties
}

export interface Edge {
  id: string;
  source: string;
  target: string;
  metadata: Record<string, any>;
  type?: string;
  weight: number;
  [key: string]: any;  // Allow additional properties
}

export interface LayoutOptions {
  padding: number;
}

export type FileType = 'json' | 'dot' | 'csv';

export const useGraphStore = defineStore('graph', {
  state: () => ({
    layoutManager: null as LayoutManager | null,
    nodes: new Map<string, Node>(),
    edges: new Map<string, Edge>(),
    filteredNodeIds: new Set<string>(),
    isLoading: false,
    error: null as string | null,
    name: '',
    description: '',
  }),

  getters: {
    // Basic data access
    getNode: (state) => (id: string) => state.nodes.get(id),
    getEdge: (state) => (id: string) => state.edges.get(id),
    
    // Array conversions for UI
    nodesArray: (state) => Array.from(state.nodes.values()),
    edgesArray: (state) => Array.from(state.edges.values()),
    
    // Filtered data
    filteredNodes: (state) => 
      state.filteredNodeIds.size > 0
        ? Array.from(state.nodes.values()).filter(n => state.filteredNodeIds.has(n.id))
        : Array.from(state.nodes.values()),
    
    filteredEdges: (state) => 
      state.filteredNodeIds.size > 0
        ? Array.from(state.edges.values())
            .filter(e => state.filteredNodeIds.has(e.source) && state.filteredNodeIds.has(e.target))
        : Array.from(state.edges.values()),
    
    // Stats
    nodeCount: (state) => state.nodes.size,
    edgeCount: (state) => state.edges.size,
    hasGraph: (state) => state.nodes.size > 0 || state.edges.size > 0,
  },

  actions: {
    // WASM Integration
    async initialize(): Promise<void> {
      try {
        const nuxtApp = useNuxtApp();
        this.layoutManager = (nuxtApp.$createLayoutManager as () => LayoutManager)();
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error);
        console.error('Failed to initialize layout manager:', error);
      }
    },

    // Graph Loading
    async loadGraphFromFile(file: File): Promise<void> {
      this.isLoading = true;
      this.error = null;

      try {
        // Initialize WASM if needed
        if (!this.layoutManager) {
          await this.initialize();
        }

        const content = await this.readFileContent(file);
        const fileType = file.name.split('.').pop()?.toLowerCase() as FileType;

        if (!['json', 'dot', 'csv'].includes(fileType)) {
          throw new Error(`Unsupported file type: ${fileType}`);
        }

        // Parse and load graph using WASM
        await this.layoutManager!.parse_and_load_graph(content, fileType);
        const graphJson = await this.layoutManager!.get_graph_json();
        const graph = JSON.parse(graphJson);

        // Update store
        this.clear();
        
        // Convert HashMaps from Rust to Maps in TypeScript
        for (const [id, node] of Object.entries<Node>(graph.nodes)) {
          this.nodes.set(id, node);
        }

        for (const [id, edge] of Object.entries<Edge>(graph.edges)) {
          this.edges.set(id, edge);
        }

        this.name = file.name.split('.')[0];
        
        // Apply any active filters
        this.applyActiveFilters();
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error);
        console.error('Error loading graph:', error);
        throw error;
      } finally {
        this.isLoading = false;
      }
    },

    // Node Operations
    addNode(node: Node): void {
      try {
        // Add to local store
        this.nodes.set(node.id, node);

        // Add to WASM
        if (this.layoutManager) {
          this.layoutManager.add_node(
            node.id,
            node.position ? node.position[0] : node.x,
            node.position ? node.position[1] : node.y
          );
        }
      } catch (error) {
        console.error('Error adding node:', error);
        throw error;
      }
    },

    removeNode(id: string): void {
      try {
        // Remove from local store
        this.nodes.delete(id);
        this.filteredNodeIds.delete(id);

        // Remove connected edges
        for (const [edgeId, edge] of this.edges) {
          if (edge.source === id || edge.target === id) {
            this.edges.delete(edgeId);
          }
        }

        // Remove from WASM
        if (this.layoutManager) {
          this.layoutManager.remove_node(id);
        }
      } catch (error) {
        console.error('Error removing node:', error);
        throw error;
      }
    },

    // Edge Operations
    addEdge(edge: Edge): void {
      try {
        // Add to local store
        this.edges.set(edge.id, edge);

        // Add to WASM
        if (this.layoutManager) {
          this.layoutManager.add_edge(edge.id, edge.source, edge.target);
        }
      } catch (error) {
        console.error('Error adding edge:', error);
        throw error;
      }
    },

    removeEdge(id: string): void {
      try {
        // Remove from local store
        this.edges.delete(id);

        // Remove from WASM
        if (this.layoutManager) {
          this.layoutManager.remove_edge(id);
        }
      } catch (error) {
        console.error('Error removing edge:', error);
        throw error;
      }
    },

    // Layout Operations
    async applyLayout(options: LayoutOptions): Promise<void> {
      if (!this.layoutManager) {
        throw new Error('WASM LayoutManager not initialized');
      }

      try {
        // Apply layout in WASM
        const updatedGraphJson = await this.layoutManager.apply_fcose_layout(
          JSON.stringify(options)
        );

        // Parse and update local store with new positions
        const updatedGraph = JSON.parse(updatedGraphJson) as { nodes: Record<string, { position: [number, number] }> };
        
        // Update node positions
        for (const [id, node] of Object.entries(updatedGraph.nodes)) {
          const existingNode = this.nodes.get(id);
          if (existingNode && node.position) {
            existingNode.position = node.position;
            existingNode.x = node.position[0];
            existingNode.y = node.position[1];
          }
        }
      } catch (error) {
        console.error('Error applying layout:', error);
        throw error;
      }
    },

    // Filtering
    applyActiveFilters(): void {
      const actionsStore = useActionsStore();
      const activeInstances = actionsStore.getActionInstances();
      
      // Start with all nodes
      let filteredNodes = Array.from(this.nodes.values());
      
      // Apply each filter action
      for (const instance of activeInstances) {
        const action = actionsStore.getActionById(instance.actionId);
        
        if (!action || !instance.state || !instance.state.filter) continue;
        
        const filter = instance.state.filter;
        
        // Apply name filter
        if (filter.type === 'name' && filter.pattern) {
          const pattern = filter.pattern.replace(/\*/g, '.*');
          const regex = new RegExp(pattern, filter.caseSensitive ? '' : 'i');
          
          filteredNodes = filteredNodes.filter(node => 
            regex.test(node.label || '')
          );
        }
        
        // Apply content filter
        else if (filter.type === 'content' && filter.pattern) {
          const regex = new RegExp(filter.pattern, filter.caseSensitive ? '' : 'i');
          
          filteredNodes = filteredNodes.filter(node => 
            Object.values(node).some(value => 
              typeof value === 'string' && regex.test(value)
            )
          );
        }
        
        // Apply tag filter
        else if (filter.type === 'tag' && filter.tags && filter.tags.length > 0) {
          filteredNodes = filteredNodes.filter(node => 
            node.tags && filter.tags.some((tag: string) => 
              Array.isArray(node.tags) && node.tags.includes(tag)
            )
          );
        }
      }
      
      // Apply search actions
      for (const instance of activeInstances) {
        const action = actionsStore.getActionById(instance.actionId);
        
        if (!action || !instance.state || !instance.state.search) continue;
        
        const search = instance.state.search;
        
        if (search.query) {
          const regex = new RegExp(search.query, 'i');
          
          filteredNodes = filteredNodes.filter(node => {
            // Search in node label
            if (regex.test(node.label || '')) return true;
            
            // Search in node content if includeContent is true
            if (search.includeContent) {
              return Object.values(node).some(value => 
                typeof value === 'string' && regex.test(value)
              );
            }
            
            return false;
          });
        }
      }
      
      // Update filtered node IDs
      this.filteredNodeIds = new Set(filteredNodes.map(node => node.id));
    },

    // File Operations
    async readFileContent(file: File): Promise<string> {
      return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result as string);
        reader.onerror = () => reject(new Error('Failed to read file'));
        reader.readAsText(file);
      });
    },

    exportGraph(format: 'json' | 'dot'): void {
      try {
        let content: string;
        let fileName: string;
        
        const graph = {
          nodes: this.nodesArray,
          edges: this.edgesArray,
          name: this.name,
          description: this.description
        };

        if (format === 'json') {
          content = JSON.stringify(graph, null, 2);
          fileName = `${this.name || 'graph'}.json`;
        } else if (format === 'dot') {
          content = this.convertToDot(graph);
          fileName = `${this.name || 'graph'}.dot`;
        } else {
          throw new Error(`Unsupported export format: ${format}`);
        }
        
        this.downloadFile(content, fileName, format === 'json' ? 'application/json' : 'text/plain');
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error);
        console.error('Error exporting graph:', error);
      }
    },

    convertToDot(graph: { nodes: Node[]; edges: Edge[]; name?: string }): string {
      const lines: string[] = [];
      
      // Graph header
      lines.push(`digraph ${graph.name || 'G'} {`);
      
      // Node definitions
      for (const node of graph.nodes) {
        const attributes = Object.entries(node)
          .filter(([key]) => !['id'].includes(key))
          .map(([key, value]) => `${key}="${value}"`)
          .join(', ');
        
        lines.push(`  ${node.id} [${attributes}];`);
      }
      
      // Edge definitions
      for (const edge of graph.edges) {
        const attributes = Object.entries(edge)
          .filter(([key]) => !['id', 'source', 'target'].includes(key))
          .map(([key, value]) => `${key}="${value}"`)
          .join(', ');
        
        if (attributes) {
          lines.push(`  ${edge.source} -> ${edge.target} [${attributes}];`);
        } else {
          lines.push(`  ${edge.source} -> ${edge.target};`);
        }
      }
      
      // Graph footer
      lines.push('}');
      
      return lines.join('\n');
    },

    downloadFile(content: string, fileName: string, mimeType: string): void {
      const blob = new Blob([content], { type: mimeType });
      const url = URL.createObjectURL(blob);
      
      const a = document.createElement('a');
      a.href = url;
      a.download = fileName;
      a.click();
      
      URL.revokeObjectURL(url);
    },

    // Cleanup
    clear(): void {
      this.nodes.clear();
      this.edges.clear();
      this.filteredNodeIds.clear();
      this.name = '';
      this.description = '';
      this.error = null;
    },

    dispose(): void {
      this.layoutManager = null;
      this.clear();
    }
  }
});
