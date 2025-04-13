<template>
  <div class="h-full overflow-y-auto">
    <div class="p-4">
      <h2 class="text-lg font-bold mb-3">Explorer</h2>
      
      <div class="mb-4">
        <div class="flex items-center gap-2 py-1 px-2 hover:bg-[var(--color-bg-tertiary)] cursor-pointer rounded">
          <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-4 h-4">
            <path d="M3 9h18v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V9Z"></path>
            <path d="M3 9V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v4"></path>
          </svg>
          <span class="text-sm">Project Files</span>
        </div>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">Folders</h3>
        <div class="pl-2">
          <div v-for="(folder, index) in folders" :key="index" class="mb-1">
            <div class="flex items-center gap-2 py-1 px-2 hover:bg-[var(--color-bg-tertiary)] cursor-pointer rounded">
              <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-4 h-4">
                <path d="M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z"></path>
              </svg>
              <span class="text-sm">{{ folder }}</span>
            </div>
          </div>
        </div>
      </div>
      
      <div class="mb-4">
        <h3 class="text-sm font-bold mb-2 text-[var(--color-text-secondary)]">Files</h3>
        <div class="pl-2">
          <div v-for="(file, index) in files" :key="index" class="mb-1">
            <div class="flex items-center gap-2 py-1 px-2 hover:bg-[var(--color-bg-tertiary)] cursor-pointer rounded">
              <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="w-4 h-4">
                <path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"></path>
                <polyline points="14 2 14 8 20 8"></polyline>
              </svg>
              <span class="text-sm">{{ file }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
// Sample data for demonstration
const folders = [
  'assets',
  'components',
  'layouts',
  'pages',
  'plugins',
  'stores'
];

const files = [
  'app.vue',
  'nuxt.config.ts',
  'package.json',
  'tailwind.config.js',
  'tsconfig.json'
];
</script>
