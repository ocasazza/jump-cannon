<template>
  <div class="custom">
    <slot name="header">
      <p>
        <i>Default #header slot content for <code>layouts/custom.vue</code></i>
      </p>
    </slot>
    <slot />
  </div>
</template>
