import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const bridge = process.env.MURMUR_BRIDGE ?? 'http://127.0.0.1:8765'

export default defineConfig({
  plugins: [react()],
  // maplibre-gl v6 loads its web worker from a sibling file; pre-bundling breaks that URL
  optimizeDeps: { exclude: ['maplibre-gl'] },
  server: {
    port: 5173,
    proxy: {
      '/api': bridge,
      '/ws': { target: bridge.replace(/^http/, 'ws'), ws: true },
    },
  },
})
