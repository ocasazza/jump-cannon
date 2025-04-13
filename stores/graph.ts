import { defineStore } from 'pinia';
import { useActionsStore } from './actions';

export interface GraphNode {
  id: string;
  label: string;
  // Additional properties that might be in the file
  [key: string]: any;
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  // Additional properties that might be in the file
  [key: string]: any;
}

export interface Graph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  // Metadata
  name?: string;
  description?: string;
}

export const useGraphStore = defineStore('graph', {
  state: () => ({
    graph: null as Graph | null,
    filteredGraph: null as Graph | null,
    isLoading: false,
    error: null as string | null,
  }),
  
  getters: {
    hasGraph: (state) => state.graph !== null,
    getGraph: (state) => state.graph,
    getFilteredGraph: (state) => state.filteredGraph || state.graph,
    getNodeCount: (state) => state.graph?.nodes.length || 0,
    getEdgeCount: (state) => state.graph?.edges.length || 0,
  },
  
  actions: {
    // Load graph from file
    async loadGraphFromFile(file: File): Promise<void> {
      this.isLoading = true;
      this.error = null;
      
      try {
        const content = await this.readFileContent(file);
        const fileExtension = file.name.split('.').pop()?.toLowerCase();
        
        let graph: Graph;
        
        if (fileExtension === 'json') {
          graph = this.parseJsonGraph(content);
        } else if (fileExtension === 'dot') {
          graph = this.parseDotGraph(content);
        } else {
          throw new Error(`Unsupported file format: ${fileExtension}`);
        }
        
        // Set the graph name from the file name
        graph.name = file.name.split('.')[0];
        
        this.graph = graph;
        this.filteredGraph = null;
        
        // Apply any active filters
        this.applyActiveFilters();
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error);
        console.error('Error loading graph:', error);
      } finally {
        this.isLoading = false;
      }
    },
    
    // Read file content as text
    async readFileContent(file: File): Promise<string> {
      return new Promise((resolve, reject) => {
        const reader = new FileReader();
        
        reader.onload = (event) => {
          if (event.target?.result) {
            resolve(event.target.result as string);
          } else {
            reject(new Error('Failed to read file'));
          }
        };
        
        reader.onerror = () => {
          reject(new Error('Error reading file'));
        };
        
        reader.readAsText(file);
      });
    },
    
    // Parse JSON graph
    parseJsonGraph(content: string): Graph {
      try {
        const data = JSON.parse(content);
        
        // Check if the JSON has the expected structure
        if (!Array.isArray(data.nodes) || !Array.isArray(data.edges)) {
          // Try to detect and convert other JSON graph formats
          if (Array.isArray(data) && data[0] && ('source' in data[0] || 'target' in data[0])) {
            // Looks like an edge list format
            return this.convertEdgeListToGraph(data);
          }
          
          throw new Error('Invalid JSON graph format: missing nodes or edges arrays');
        }
        
        // Ensure all nodes have an id and label
        const nodes = data.nodes.map((node: any, index: number) => ({
          id: node.id || `node-${index}`,
          label: node.label || node.name || node.id || `Node ${index}`,
          ...node
        }));
        
        // Ensure all edges have an id, source, and target
        const edges = data.edges.map((edge: any, index: number) => ({
          id: edge.id || `edge-${index}`,
          source: edge.source,
          target: edge.target,
          ...edge
        }));
        
        return {
          nodes,
          edges,
          name: data.name,
          description: data.description
        };
      } catch (error) {
        throw new Error(`Error parsing JSON: ${error instanceof Error ? error.message : String(error)}`);
      }
    },
    
    // Convert edge list to graph
    convertEdgeListToGraph(edgeList: any[]): Graph {
      const nodeMap = new Map<string, GraphNode>();
      const edges: GraphEdge[] = [];
      
      // Extract nodes and edges from the edge list
      edgeList.forEach((edge, index) => {
        const sourceId = String(edge.source);
        const targetId = String(edge.target);
        
        // Add source node if it doesn't exist
        if (!nodeMap.has(sourceId)) {
          nodeMap.set(sourceId, {
            id: sourceId,
            label: edge.sourceLabel || sourceId
          });
        }
        
        // Add target node if it doesn't exist
        if (!nodeMap.has(targetId)) {
          nodeMap.set(targetId, {
            id: targetId,
            label: edge.targetLabel || targetId
          });
        }
        
        // Add edge
        edges.push({
          id: edge.id || `edge-${index}`,
          source: sourceId,
          target: targetId,
          ...edge
        });
      });
      
      return {
        nodes: Array.from(nodeMap.values()),
        edges
      };
    },
    
    // Parse DOT graph
    parseDotGraph(content: string): Graph {
      try {
        // Simple DOT parser (for basic DOT files)
        const nodes: GraphNode[] = [];
        const edges: GraphEdge[] = [];
        const nodeMap = new Map<string, GraphNode>();
        
        // Extract graph name
        const graphNameMatch = content.match(/(?:digraph|graph)\s+([a-zA-Z0-9_]+)/);
        const graphName = graphNameMatch ? graphNameMatch[1] : 'Untitled';
        
        // Extract nodes and edges
        const lines = content.split('\n');
        
        for (const line of lines) {
          // Skip comments and empty lines
          if (line.trim().startsWith('//') || line.trim().startsWith('#') || line.trim() === '') {
            continue;
          }
          
          // Extract node definitions
          const nodeMatch = line.match(/\s*([a-zA-Z0-9_]+)\s*\[(.+)\]/);
          if (nodeMatch) {
            const nodeId = nodeMatch[1];
            const attributes = this.parseDotAttributes(nodeMatch[2]);
            
            const node: GraphNode = {
              id: nodeId,
              label: attributes.label || nodeId,
              ...attributes
            };
            
            nodes.push(node);
            nodeMap.set(nodeId, node);
            continue;
          }
          
          // Extract edge definitions
          const edgeMatch = line.match(/\s*([a-zA-Z0-9_]+)\s*(->|--)\s*([a-zA-Z0-9_]+)(?:\s*\[(.+)\])?/);
          if (edgeMatch) {
            const sourceId = edgeMatch[1];
            const targetId = edgeMatch[3];
            const attributes = edgeMatch[4] ? this.parseDotAttributes(edgeMatch[4]) : {};
            
            // Add nodes if they don't exist
            if (!nodeMap.has(sourceId)) {
              const node: GraphNode = {
                id: sourceId,
                label: sourceId
              };
              nodes.push(node);
              nodeMap.set(sourceId, node);
            }
            
            if (!nodeMap.has(targetId)) {
              const node: GraphNode = {
                id: targetId,
                label: targetId
              };
              nodes.push(node);
              nodeMap.set(targetId, node);
            }
            
            // Add edge
            edges.push({
              id: `${sourceId}-${targetId}`,
              source: sourceId,
              target: targetId,
              ...attributes
            });
          }
        }
        
        return {
          nodes,
          edges,
          name: graphName
        };
      } catch (error) {
        throw new Error(`Error parsing DOT: ${error instanceof Error ? error.message : String(error)}`);
      }
    },
    
    // Parse DOT attributes
    parseDotAttributes(attributesStr: string): Record<string, any> {
      const attributes: Record<string, any> = {};
      
      // Match attribute pairs like key="value" or key=value
      const attributeRegex = /([a-zA-Z0-9_]+)\s*=\s*(?:"([^"]*)"|\{([^}]*)\}|([a-zA-Z0-9_]+))/g;
      let match;
      
      while ((match = attributeRegex.exec(attributesStr)) !== null) {
        const key = match[1];
        // Use the first non-undefined value from the capture groups
        const value = match[2] !== undefined ? match[2] : (match[3] !== undefined ? match[3] : match[4]);
        attributes[key] = value;
      }
      
      return attributes;
    },
    
    // Export graph to file
    exportGraph(format: 'json' | 'dot'): void {
      if (!this.graph) {
        this.error = 'No graph to export';
        return;
      }
      
      try {
        let content: string;
        let fileName: string;
        
        if (format === 'json') {
          content = JSON.stringify(this.graph, null, 2);
          fileName = `${this.graph.name || 'graph'}.json`;
        } else if (format === 'dot') {
          content = this.convertToDot(this.graph);
          fileName = `${this.graph.name || 'graph'}.dot`;
        } else {
          throw new Error(`Unsupported export format: ${format}`);
        }
        
        this.downloadFile(content, fileName, format === 'json' ? 'application/json' : 'text/plain');
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error);
        console.error('Error exporting graph:', error);
      }
    },
    
    // Convert graph to DOT format
    convertToDot(graph: Graph): string {
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
    
    // Download file
    downloadFile(content: string, fileName: string, mimeType: string): void {
      const blob = new Blob([content], { type: mimeType });
      const url = URL.createObjectURL(blob);
      
      const a = document.createElement('a');
      a.href = url;
      a.download = fileName;
      a.click();
      
      URL.revokeObjectURL(url);
    },
    
    // Apply active filters from the actions store
    applyActiveFilters(): void {
      if (!this.graph) return;
      
      const actionsStore = useActionsStore();
      const activeInstances = actionsStore.getActionInstances();
      
      // Start with the original graph
      let filteredNodes = [...this.graph.nodes];
      
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
      
      // Get the IDs of filtered nodes
      const filteredNodeIds = new Set(filteredNodes.map(node => node.id));
      
      // Filter edges to only include those connecting filtered nodes
      const filteredEdges = this.graph.edges.filter(edge => 
        filteredNodeIds.has(edge.source) && filteredNodeIds.has(edge.target)
      );
      
      // Update filtered graph
      this.filteredGraph = {
        nodes: filteredNodes,
        edges: filteredEdges,
        name: this.graph.name,
        description: this.graph.description
      };
    },
    
    // Clear the current graph
    clearGraph(): void {
      this.graph = null;
      this.filteredGraph = null;
      this.error = null;
    }
  }
});
