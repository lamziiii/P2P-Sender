import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  base: './',
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: 'es2022', chunkSizeWarningLimit: 1000 },
})
