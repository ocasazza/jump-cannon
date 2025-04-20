import { defineNuxtPlugin } from 'nuxt/app'
import { LayoutManager, set_panic_hook } from '~/wasm/rust-graph-layouts/pkg/rust_graph_layouts'

export default defineNuxtPlugin(async () => {
  try {
    set_panic_hook()
    console.log('WASM module initialized successfully')

    return {
      provide: {
        createLayoutManager: (): LayoutManager => {
          try {
            return new LayoutManager()
          } catch (error) {
            console.error('Failed to create LayoutManager:', error)
            throw error
          }
        }
      }
    }
  } catch (error) {
    console.error('Failed to initialize WASM module:', error)
    throw error
  }
})
