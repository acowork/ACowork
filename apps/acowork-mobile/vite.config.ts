import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The mobile app is a browser-first PWA shell. In Tauri the same bundle runs
// inside the native webview, so no platform-specific Vite config is needed.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 19877,
    strictPort: true,
  },
  build: {
    target: 'es2022',
    sourcemap: false,
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
