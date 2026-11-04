import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import pkg from './package.json' with { type: 'json' }

// The version as shown and in CHANGELOG.md: a fourth number, semver build
// metadata in package.json (3.4.7+2), is shown as one (3.4.7.2).
const shownVersion = pkg.version.replace(/^(\d+\.\d+\.\d+)\+(\d+)$/, '$1.$2')

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  base: './',
  plugins: [react()],
  define: { __APP_VERSION__: JSON.stringify(shownVersion) },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: 'es2022', chunkSizeWarningLimit: 1000 },
})
