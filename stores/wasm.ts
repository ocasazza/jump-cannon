import { defineStore } from 'pinia'
import { LayoutManager, set_panic_hook } from '~/wasm/rust-graph-layouts/pkg/rust_graph_layouts'

export const useWasmStore = defineStore('wasm', () => {
  async function initialize(): Promise<void> {
    set_panic_hook()
  }

  function dispose() {}

  return {
    LayoutManager,
    dispose,
    initialize,
  }
});
