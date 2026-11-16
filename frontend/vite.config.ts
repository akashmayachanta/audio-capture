import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vitest/config'

// Built into dist/ and embedded in meetrec.exe (see backend/recorder/build.rs).
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  test: { include: ['src/**/*.test.ts'] },
})
