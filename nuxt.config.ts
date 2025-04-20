import wasm from 'vite-plugin-wasm'
import topLevelAwait from 'vite-plugin-top-level-await'
import { defineNuxtConfig } from 'nuxt/config'
import { resolve } from 'path'

// https://nuxt.com/docs/api/configuration/nuxt-config
export default defineNuxtConfig({
  typescript: {
    strict: false,
    typeCheck: false,
    shim: false,
  },

  alias: {
    '#app': resolve(__dirname, 'node_modules/nuxt/dist/app'),
  },

  ssr: false,

  app: {
    head: {
      title: 'jump-cannon',
      meta: [
        { charset: 'utf-8' },
        { name: 'viewport', content: 'width=device-width, initial-scale=1' },
        { name: 'format-detection', content: 'telephone=no' }
      ],
      link: [
        { rel: 'icon', type: 'image/svg', href: '/favicon.svg' }
      ]
    },
    baseURL: '/jump-cannon/',
    buildAssetsDir: 'assets'
  },

  modules: [
    '@pinia/nuxt',
    '@nuxtjs/tailwindcss',
  ],

  css: [
    '~/assets/style/main.scss',
  ],

  plugins: [
    '~/plugins/wasm',
    '~/plugins/register-actions',
  ],

  nitro: {
    prerender: {
      // Workaround for "Error: [404] Page not found: /manifest.json"
      failOnError: false,
    },
    compatibilityDate: '2025-04-16',
  },

  vite: {
    plugins: [
      wasm(),
      topLevelAwait()
    ],
    build: {
      target: 'esnext'
    }
  },

  compatibilityDate: '2025-04-16',
})