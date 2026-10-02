import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const bridge = process.env.MURMUR_BRIDGE ?? 'http://127.0.0.1:8765'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': bridge,
      '/ws': { target: bridge.replace(/^http/, 'ws'), ws: true },
    },
  },
})
