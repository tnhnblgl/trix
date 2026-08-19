import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // Fixed port: tauri.conf.json's devUrl points at it, and a port that moves
  // when 1420 is busy would leave `cargo tauri dev` staring at a blank window.
  //
  // `host` spelled out as the literal 127.0.0.1, and `devUrl` names the same
  // literal rather than `localhost`, because on a Windows box where `localhost`
  // resolves to ::1 first the two halves disagree: Vite's default binding is
  // IPv4 only, the webview asks for ::1, and the window shows "localhost
  // refused to connect" with a perfectly healthy dev server running. Naming one
  // address on both sides takes the resolver out of it entirely. Not `true` or
  // `0.0.0.0`, which would also work and would additionally serve the app to
  // anyone on the network.
  server: { host: '127.0.0.1', port: 1420, strictPort: true },
  build: { target: 'esnext', emptyOutDir: true },
});
