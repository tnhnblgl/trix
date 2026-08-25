<script lang="ts">
  import { startDaemon } from '../lib/ipc';
  import { app } from '../lib/state.svelte';
  import Button from '../components/ui/Button.svelte';

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
  <Button variant="primary" disabled={starting} onclick={start}>
    {starting ? 'Starting...' : 'Start Trix'}
  </Button>
</div>

<style>
  .down {
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 12px;
    flex: 1;
    text-align: center;
    padding: 40px;
  }
  h2 { margin: 0; font-size: 18px; font-weight: 650; }
  p { margin: 0 0 6px; max-width: 42ch; color: var(--dim); font-size: 12.5px; }
</style>
