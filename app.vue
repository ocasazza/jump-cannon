<template>
    <NuxtLoadingIndicator />
    <NuxtLayout>
      <NuxtPage />
    </NuxtLayout>
</template>

<script setup>
import init, {greet} from "wasm/jump-cannon"

init()
greet()
</script>