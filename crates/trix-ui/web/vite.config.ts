import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // Fixed port: tauri.conf.json's devUrl points at it, and a port that moves
  // when 1420 is busy would leave `cargo tauri dev` staring at a blank window.
  server: { port: 1420, strictPort: true },
  build: { target: 'esnext', emptyOutDir: true },
});
