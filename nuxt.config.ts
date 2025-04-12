import wasmPack from 'vite-plugin-wasm-pack'

// https://nuxt.com/docs/api/configuration/nuxt-config
export default defineNuxtConfig({
  ssr: false,
  app: {
    head: {
      title: 'kbg',
      meta: [
        { charset: 'utf-8' },
        { name: 'viewport', content: 'width=device-width, initial-scale=1' },
        { hid: 'description', name: 'description', content: 'Dario Tecchia\' personal website!' },
        { name: 'format-detection', content: 'telephone=no' }
      ],
      link: [
        { rel: 'icon', type: 'image/svg', href: '/favicon.svg' }
      ]
    },
    baseURL: ''
  },
  devtools: { enabled: true },
  modules: [
    // '@nuxt/content',
    // '@nuxt/eslint',
    // '@nuxt/fonts',
    // '@nuxt/icon',
    // '@nuxt/image',
    // '@nuxt/scripts',
    // '@nuxt/test-utils'
  ],
  nitro: {
    prerender: {
      // Workaround for "Error: [404] Page not found: /manifest.json"
      failOnError: false,
    },
  },
  vite: {
    plugins: [
      wasmPack(['./wasm/jump-cannon'])
    ]
  },
})