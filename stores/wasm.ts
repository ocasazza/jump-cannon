import { defineStore } from 'pinia'
import initJumpCannon, { greet } from '~/wasm/jump-cannon/pkg/jump_cannon'

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