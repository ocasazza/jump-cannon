import { defineStore, StoreState } from 'pinia'
import { useGraphCoreStore } from './core'

export interface SelectionState {
  selectedNodes: Set<string>
  selectedEdges: Set<string>
  hoveredNode: string | null
  hoveredEdge: string | null
}

interface SelectionActions {
  selectNode(nodeId: string, clearOthers?: boolean): void
  selectEdge(edgeId: string, clearOthers?: boolean): void
  deselectNode(nodeId: string): void
  deselectEdge(edgeId: string): void
  toggleNodeSelection(nodeId: string): void
  toggleEdgeSelection(edgeId: string): void
  setHoveredNode(nodeId: string | null): void
  setHoveredEdge(edgeId: string | null): void
  clearSelection(): void
  clearHovered(): void
  selectConnectedNodes(): void
  selectEdgesBetweenNodes(): void
}

type SelectionStore = SelectionState & SelectionActions

export const useGraphSelectionStore = defineStore('graphSelection', {
  state: (): SelectionState => ({
    selectedNodes: new Set<string>(),
    selectedEdges: new Set<string>(),
    hoveredNode: null,
    hoveredEdge: null
  }),

  getters: {
    hasSelection: (state: StoreState<SelectionState>): boolean => 
      state.selectedNodes.size > 0 || state.selectedEdges.size > 0,
    
    isNodeSelected: (state: StoreState<SelectionState>) => (nodeId: string): boolean => 
      state.selectedNodes.has(nodeId),
    
    isEdgeSelected: (state: StoreState<SelectionState>) => (edgeId: string): boolean => 
      state.selectedEdges.has(edgeId),
    
    getSelectedNodes(state: StoreState<SelectionState>): string[] {
      return Array.from(state.selectedNodes)
    },
    
    getSelectedEdges(state: StoreState<SelectionState>): string[] {
      return Array.from(state.selectedEdges)
    },

    getHoveredNode: (state: StoreState<SelectionState>): string | null => state.hoveredNode,
    
    getHoveredEdge: (state: StoreState<SelectionState>): string | null => state.hoveredEdge
  },

  actions: {
    selectNode(this: SelectionStore, nodeId: string, clearOthers = true): void {
      const graphStore = useGraphCoreStore()
      if (!graphStore.getNode(nodeId)) return

      if (clearOthers) {
        this.clearSelection()
      }
      this.selectedNodes.add(nodeId)
    },

    selectEdge(this: SelectionStore, edgeId: string, clearOthers = true): void {
      const graphStore = useGraphCoreStore()
      if (!graphStore.getEdge(edgeId)) return

      if (clearOthers) {
        this.clearSelection()
      }
      this.selectedEdges.add(edgeId)
    },

    deselectNode(this: SelectionStore, nodeId: string): void {
      this.selectedNodes.delete(nodeId)
    },

    deselectEdge(this: SelectionStore, edgeId: string): void {
      this.selectedEdges.delete(edgeId)
    },

    toggleNodeSelection(this: SelectionStore, nodeId: string): void {
      if (this.selectedNodes.has(nodeId)) {
        this.deselectNode(nodeId)
      } else {
        this.selectNode(nodeId)
      }
    },

    toggleEdgeSelection(this: SelectionStore, edgeId: string): void {
      if (this.selectedEdges.has(edgeId)) {
        this.deselectEdge(edgeId)
      } else {
        this.selectEdge(edgeId)
      }
    },

    setHoveredNode(this: SelectionStore, nodeId: string | null): void {
      this.hoveredNode = nodeId
    },

    setHoveredEdge(this: SelectionStore, edgeId: string | null): void {
      this.hoveredEdge = edgeId
    },

    clearSelection(this: SelectionStore): void {
      this.selectedNodes.clear()
      this.selectedEdges.clear()
    },

    clearHovered(this: SelectionStore): void {
      this.hoveredNode = null
      this.hoveredEdge = null
    },

    // Select nodes connected to the currently selected nodes
    selectConnectedNodes(this: SelectionStore): void {
      const graphStore = useGraphCoreStore()
      const connectedNodes = new Set<string>()

      // Find all nodes connected to selected nodes via edges
      const edges = graphStore.getAllEdges()
      for (const edge of edges) {
        if (this.selectedNodes.has(edge.source)) {
          connectedNodes.add(edge.target)
        }
        if (this.selectedNodes.has(edge.target)) {
          connectedNodes.add(edge.source)
        }
      }

      // Add connected nodes to selection
      for (const nodeId of connectedNodes) {
        this.selectedNodes.add(nodeId)
      }
    },

    // Select all edges between currently selected nodes
    selectEdgesBetweenNodes(this: SelectionStore): void {
      const graphStore = useGraphCoreStore()

      // Find all edges connecting selected nodes
      const edges = graphStore.getAllEdges()
      for (const edge of edges) {
        if (this.selectedNodes.has(edge.source) && 
            this.selectedNodes.has(edge.target)) {
          this.selectedEdges.add(edge.id)
        }
      }
    }
  }
})
