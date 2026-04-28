import { defineNuxtPlugin } from 'nuxt/app'
import __wbg_init, { set_panic_hook, LayoutManager } from '~/wasm/rust-graph-layouts/pkg/rust_graph_layouts'

export default defineNuxtPlugin({
  name: 'wasm',
  enforce: 'pre',
  async setup (nuxtApp) {
    try {
      await __wbg_init()
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
  }
})
