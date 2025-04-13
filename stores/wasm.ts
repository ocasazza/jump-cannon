import { defineStore } from 'pinia'
import initJumpCannon, { greet } from '~/wasm/jump-cannon/pkg/jump_cannon'

export const useWasmStore = defineStore('wasm', () => {
  const isLoaded = ref(false);

  async function init (): Promise<void> {
    await initJumpCannon()
    isLoaded.value = true;
  }

  function dispose () {}

  return {
    greet,
    dispose,
    init,
    isLoaded,
  }
});