/**
 * Separate config for the one billable live test, so the default
 * the default include glob in vite.config.ts (src only) can never collect it.
 * Only `npx vitest run --config dev/vitest.send.config.ts` reaches it.
 */

import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    include: ['dev/live-send.test.ts'],
  },
})
