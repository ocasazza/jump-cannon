import { defineStore } from 'pinia'
import { useNuxtApp } from 'nuxt/app'
import type { LayoutManager } from '~/wasm/rust-graph-layouts/pkg/rust-graph-layouts'

export interface Node {
  id: string
  label: string
  position?: [number, number]
  metadata: Record<string, any>
  type?: string
  x: number
  y: number
}

export interface Edge {
  id: string
  source: string
  target: string
  metadata: Record<string, any>
  type?: string
  weight: number
}

export interface Graph {
  nodes: { [key: string]: Node }
  edges: { [key: string]: Edge }
  name?: string
  description?: string
}

export interface LayoutOptions {
  padding: number
}

export type FileType = 'json' | 'dot' | 'csv'

export const useGraphCoreStore = defineStore('graphCore', {
  state: () => ({
    layoutManager: null as LayoutManager | null,
    nodes: new Map<string, Node>(),
    edges: new Map<string, Edge>(),
    isLoading: false,
    error: null as string | null,
  }),

  getters: {
    getNode: (state) => (id: string) => state.nodes.get(id),
    getEdge: (state) => (id: string) => state.edges.get(id),
    getAllNodes: (state) => Array.from(state.nodes.values()),
    getAllEdges: (state) => Array.from(state.edges.values()),
    getNodeCount: (state) => state.nodes.size,
    getEdgeCount: (state) => state.edges.size,
  },

  actions: {
    async initialize(): Promise<void> {
      try {
        const nuxtApp = useNuxtApp()
        this.layoutManager = (nuxtApp.$createLayoutManager as () => LayoutManager)()
      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error)
        console.error('Failed to initialize layout manager:', error)
      }
    },

    async loadGraphFromFile(file: File): Promise<void> {
      this.isLoading = true
      this.error = null

      try {
        // Initialize WASM if needed
        if (!this.layoutManager) {
          await this.initialize()
        }

        // Read file content
        const content = await this.readFileContent(file)
        const fileType = file.name.split('.').pop()?.toLowerCase() as FileType

        if (!['json', 'dot', 'csv'].includes(fileType)) {
          throw new Error(`Unsupported file type: ${fileType}`)
        }

        // Parse and load graph using WASM
        await this.layoutManager.parse_and_load_graph(content, fileType)

        // Get the loaded graph data
        const graphJson = await this.layoutManager.get_graph_json()
        const graph = JSON.parse(graphJson)

        // Update local store
        this.nodes.clear()
        this.edges.clear()

        // Convert HashMaps from Rust to Maps in TypeScript
        for (const [id, node] of Object.entries(graph.nodes)) {
          this.nodes.set(id, node)
        }

        for (const [id, edge] of Object.entries(graph.edges)) {
          this.edges.set(id, edge)
        }

      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error)
        console.error('Error loading graph:', error)
        throw error
      } finally {
        this.isLoading = false
      }
    },

    async readFileContent(file: File): Promise<string> {
      return new Promise((resolve, reject) => {
        const reader = new FileReader()
        reader.onload = () => resolve(reader.result as string)
        reader.onerror = () => reject(new Error('Failed to read file'))
        reader.readAsText(file)
      })
    },

    addNode(node: Node): void {
      try {
        // Add to local store
        this.nodes.set(node.id, node)

        // Add to WASM
        if (this.layoutManager) {
          this.layoutManager.add_node(
            node.id,
            node.position ? node.position[0] : node.x,
            node.position ? node.position[1] : node.y
          )
        }
      } catch (error) {
        console.error('Error adding node:', error)
        throw error
      }
    },

    addEdge(edge: Edge): void {
      try {
        // Add to local store
        this.edges.set(edge.id, edge)

        // Add to WASM
        if (this.layoutManager) {
          this.layoutManager.add_edge(edge.id, edge.source, edge.target)
        }
      } catch (error) {
        console.error('Error adding edge:', error)
        throw error
      }
    },

    removeNode(id: string): void {
      try {
        // Remove from local store
        this.nodes.delete(id)

        // Remove connected edges
        for (const [edgeId, edge] of this.edges) {
          if (edge.source === id || edge.target === id) {
            this.edges.delete(edgeId)
          }
        }

        // Remove from WASM
        if (this.layoutManager) {
          this.layoutManager.remove_node(id)
        }
      } catch (error) {
        console.error('Error removing node:', error)
        throw error
      }
    },

    removeEdge(id: string): void {
      try {
        // Remove from local store
        this.edges.delete(id)

        // Remove from WASM
        if (this.layoutManager) {
          this.layoutManager.remove_edge(id)
        }
      } catch (error) {
        console.error('Error removing edge:', error)
        throw error
      }
    },

    async applyLayout(options: LayoutOptions): Promise<void> {
      if (!this.layoutManager) {
        throw new Error('WASM LayoutManager not initialized')
      }

      try {
        // Apply layout in WASM
        const updatedGraphJson = await this.layoutManager.apply_fcose_layout(
          JSON.stringify(options)
        )

        // Parse and update local store with new positions
        const updatedGraph = JSON.parse(updatedGraphJson) as { nodes: Record<string, { position: [number, number] }> }
        
        // Update node positions
        for (const [id, node] of Object.entries(updatedGraph.nodes)) {
          const existingNode = this.nodes.get(id)
          if (existingNode && node.position) {
            existingNode.position = node.position
            existingNode.x = node.position[0]
            existingNode.y = node.position[1]
          }
        }
      } catch (error) {
        console.error('Error applying layout:', error)
        throw error
      }
    },

    async exportGraph(): Promise<Graph> {
      const nodesObj: { [key: string]: Node } = {}
      const edgesObj: { [key: string]: Edge } = {}

      for (const [id, node] of this.nodes) {
        nodesObj[id] = node
      }

      for (const [id, edge] of this.edges) {
        edgesObj[id] = edge
      }

      return {
        nodes: nodesObj,
        edges: edgesObj
      }
    },

    dispose(): void {
      // Clean up WASM resources
      this.layoutManager = null
      this.nodes.clear()
      this.edges.clear()
    }
  }
})
