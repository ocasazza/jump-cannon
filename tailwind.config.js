/** @type {import('tailwindcss').Config} */
module.exports = {
  content: [
    './components/**/*.{js,vue,ts}',
    './layouts/**/*.vue',
    './pages/**/*.vue',
    './plugins/**/*.{js,ts}',
    './app.vue',
    './nuxt.config.{js,ts}',
  ],
  darkMode: 'class',
  theme: {
    extend: {
      colors: {
        // Theme colors
        'bg': {
          primary: '#ffffff',
          secondary: '#f3f3f3',
          tertiary: '#e5e5e5',
          dark: {
            primary: '#1e1e1e',
            secondary: '#252526',
            tertiary: '#333333',
          }
        },
        'text': {
          primary: '#333333',
          secondary: '#5a5a5a',
          tertiary: '#767676',
          dark: {
            primary: '#ffffff',
            secondary: '#cccccc',
            tertiary: '#9d9d9d',
          }
        },
        'border': {
          DEFAULT: '#d4d4d4',
          active: '#0078D4',
          dark: {
            DEFAULT: '#474747',
            active: '#0078D4',
          }
        },
        // Semantic colors
        'primary': {
          DEFAULT: '#0078D4', // VSCode blue
          '50': '#E6F2FA',
          '100': '#CCE5F5',
          '200': '#99CBEB',
          '300': '#66B2E0',
          '400': '#3398D6',
          '500': '#0078D4', // Primary
          '600': '#0062AB',
          '700': '#004B83',
          '800': '#00315A',
          '900': '#001A2E',
        },
        'secondary': {
          DEFAULT: '#2C2C32', // VSCode dark gray
          '50': '#E8E8E9',
          '100': '#D1D1D3',
          '200': '#A3A3A7',
          '300': '#75757B',
          '400': '#47474F',
          '500': '#2C2C32', // Secondary
          '600': '#232328',
          '700': '#1A1A1E',
          '800': '#121214',
          '900': '#09090A',
        },
        'accent': {
          DEFAULT: '#1AAF5C', // Green
          '50': '#E7F8EF',
          '100': '#D0F1DF',
          '200': '#A1E3BF',
          '300': '#71D59F',
          '400': '#42C77F',
          '500': '#1AAF5C', // Accent
          '600': '#158C4A',
          '700': '#106937',
          '800': '#0A4625',
          '900': '#052312',
        },
        'error': {
          DEFAULT: '#e51400',
          dark: '#f48771',
        },
        'warning': {
          DEFAULT: '#ff8c00',
          dark: '#cca700',
        },
        'info': {
          DEFAULT: '#0078D4',
          dark: '#75beff',
        },
      },
      fontFamily: {
        'mono': ['Menlo', 'Monaco', 'Consolas', 'monospace'],
        'sans': ['Segoe UI', 'system-ui', 'sans-serif'],
      },
    },
  },
  plugins: [],
}
