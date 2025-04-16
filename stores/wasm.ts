import { defineStore } from 'pinia'
import initJumpCannon, { greet } from '~/wasm/rust-graph-layouts/pkg/rust-graph-layouts'

export const useWasmStore = defineStore('wasm', () => {

  async function initialize (): Promise<void> {
    await initJumpCannon()
  }

  function dispose () {}

  return {
    greet,
    dispose,
    initialize,
  }
});