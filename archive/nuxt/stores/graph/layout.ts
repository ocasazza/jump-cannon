import { defineStore } from 'pinia'
import { useGraphCoreStore } from './core'

export interface LayoutOptions {
  padding: number
  // fCoSE specific options
  idealEdgeLength?: number
  nodeSeparation?: number
  springForce?: number
  repulsionForce?: number
  gravity?: number
  numIter?: number
  tile?: boolean
}

export type LayoutAlgorithm = 'fcose' | 'concentric' | 'dagre'

export interface LayoutState {
  currentLayout: LayoutAlgorithm
  options: LayoutOptions
  isLayoutRunning: boolean
  error: string | null
  layoutHistory: LayoutAlgorithm[]
}

export const useGraphLayoutStore = defineStore('graphLayout', {
  state: (): LayoutState => ({
    currentLayout: 'fcose',
    options: {
      padding: 50,
      idealEdgeLength: 50,
      nodeSeparation: 50,
      springForce: 0.45,
      repulsionForce: 1.0,
      gravity: 0.25,
      numIter: 500,
      tile: true
    },
    isLayoutRunning: false,
    error: null,
    layoutHistory: []
  }),

  getters: {
    getCurrentLayout: (state): LayoutAlgorithm => state.currentLayout,
    getLayoutOptions: (state): LayoutOptions => state.options,
    isRunning: (state): boolean => state.isLayoutRunning,
    getError: (state): string | null => state.error,
    getLayoutHistory: (state): LayoutAlgorithm[] => state.layoutHistory
  },

  actions: {
    async applyLayout(algorithm?: LayoutAlgorithm, options?: Partial<LayoutOptions>): Promise<void> {
      const graphStore = useGraphCoreStore()
      
      if (!graphStore.layoutManager) {
        throw new Error('Layout manager not initialized')
      }

      try {
        this.isLayoutRunning = true
        this.error = null

        // Update layout algorithm if specified
        if (algorithm) {
          this.currentLayout = algorithm
        }

        // Merge provided options with defaults
        if (options) {
          this.options = { ...this.options, ...options }
        }

        // Apply the layout using the core store
        await graphStore.applyLayout(this.options)

        // Record in history
        this.layoutHistory.push(this.currentLayout)

      } catch (error) {
        this.error = error instanceof Error ? error.message : String(error)
        console.error('Error applying layout:', error)
        throw error
      } finally {
        this.isLayoutRunning = false
      }
    },

    setLayoutOptions(options: Partial<LayoutOptions>): void {
      this.options = { ...this.options, ...options }
    },

    async resetLayout(): Promise<void> {
      // Reset to default options
      this.options = {
        padding: 50,
        idealEdgeLength: 50,
        nodeSeparation: 50,
        springForce: 0.45,
        repulsionForce: 1.0,
        gravity: 0.25,
        numIter: 500,
        tile: true
      }

      // Apply layout with reset options
      await this.applyLayout(this.currentLayout, this.options)
    },

    clearHistory(): void {
      this.layoutHistory = []
    },

    undoLayout(): void {
      if (this.layoutHistory.length > 1) {
        // Remove current layout from history
        this.layoutHistory.pop()
        // Get previous layout
        const previousLayout = this.layoutHistory[this.layoutHistory.length - 1]
        // Apply previous layout
        this.applyLayout(previousLayout)
      }
    }
  }
})
