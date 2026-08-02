<script lang="ts">
  import { startDaemon } from '../lib/ipc';
  import { app } from '../lib/state.svelte';

  let starting = $state(false);

  async function start() {
    starting = true;
    try {
      await startDaemon();
    } catch (e) {
      app.toast('error', String(e));
      starting = false;
    }
    // Deliberately not cleared on success: the supervisor reconnects on its
    // own and this whole panel disappears when it does.
  }
</script>

<div class="down">
  <h2>Trix isn't running</h2>
  <p>The background service owns the replay buffer and the clip hotkey. Nothing is being captured right now.</p>
  <button onclick={start} disabled={starting}>{starting ? 'Starting...' : 'Start Trix'}</button>
</div>

<style>
  .down { display: grid; place-content: center; justify-items: center; gap: 12px; height: 100vh; text-align: center; padding: 40px; }
  h2 { margin: 0; font-size: 20px; }
  p { margin: 0; max-width: 42ch; color: var(--dim); }
  button { padding: 10px 18px; border-radius: 8px; border: 1px solid var(--accent); background: var(--accent); color: #06121f; font: inherit; font-weight: 600; cursor: pointer; }
  button:disabled { opacity: 0.6; cursor: default; }
</style>
