<script lang="ts">
  import { app } from '../lib/state.svelte';
  import { shotUrl } from '../lib/shots';
  import Button from './ui/Button.svelte';

  let { onclose }: { onclose: () => void } = $props();

  const shot = $derived(app.shots[app.shotSelected] ?? null);
  const dir = $derived(app.status?.clip_dir ?? '');

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
      return;
    }
    // Arrows walk the set without returning to the grid, which is what makes
    // this a viewer rather than a modal reopened for every image.
    const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (step === 0) return;
    e.preventDefault();
    const next = app.shotSelected + step;
    if (next >= 0 && next < app.shots.length) app.shotSelected = next;
  }
</script>

<svelte:window on:keydown={onkeydown} />

{#if shot}
  <div class="viewer">
    <img src={dir ? shotUrl(dir, shot.id) : ''} alt={`Screenshot from ${new Date(shot.created).toLocaleString()}`} />
    <div class="bar">
      <span class="tnum">{app.shotSelected + 1} / {app.shots.length}</span>
      <span class="spacer"></span>
      <Button size="sm" onclick={() => app.copyShot(shot.id)}>Copy</Button>
      <Button size="sm" onclick={() => app.revealShot(shot.id)}>Show in folder</Button>
      <Button size="sm" variant="primary" onclick={onclose}>Close</Button>
    </div>
  </div>
{/if}

<style>
  /*
   * `fixed`, not `absolute`. `Shots.svelte`'s `.page` sits inside
   * `App.svelte`'s `.content`, which is the pane that actually scrolls
   * (`overflow: auto`) -- `.page` itself is only `min-height: 100%` and grows
   * past the window whenever the grid has enough rows to need a scrollbar.
   * An `absolute; inset: 0` viewer sizes to *that* box, not to the window: on
   * a library long enough to scroll, the image would stretch to the full
   * grid height and the control bar would land off-screen below the fold.
   * `fixed` anchors to the viewport instead, which is what `Modal.svelte`'s
   * scrim and `Toasts.svelte` already do for the same reason. Checked the
   * ancestor chain for a `transform`/`filter`/`will-change` that would trap
   * it -- `.app`, `.shell`, `.content` and `TitleBar` set none, so nothing
   * here silently confines it back to a container.
   */
  .viewer {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    z-index: 5;
  }
  .viewer img { flex: 1; min-height: 0; object-fit: contain; background: #000; }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    border-top: 1px solid var(--line);
  }
  .spacer { margin-left: auto; }
</style>
