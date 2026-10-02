import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// the bridge the dev server proxies to (up.sh sets MURMUR_BRIDGE_URL from MURMUR_HTTP_PORT)
const bridge = process.env.MURMUR_BRIDGE_URL ?? `http://127.0.0.1:${process.env.MURMUR_HTTP_PORT ?? 8765}`

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
