import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';
import { devData, devOptions, devProxy } from './dev/devdata';

// The mirror serves the built panel at the root, so asset URLs resolve there.
// `vite dev` takes its data from a local mirror, from production read only, or
// from a snapshot of production's public data: see dev/devdata.ts and
// docs/development.md.
const dev = devOptions(process.env, fileURLToPath(new URL('.', import.meta.url)));

export default defineConfig({
  base: '/',
  plugins: [svelte(), devData(dev)],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    proxy: devProxy(dev),
  },
});
