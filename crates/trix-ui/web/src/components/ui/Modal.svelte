<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title,
    onclose,
    children,
    actions,
  }: {
    title: string;
    onclose: () => void;
    children: Snippet;
    actions: Snippet;
  } = $props();

  let panel = $state<HTMLDivElement | null>(null);
  /** Whatever had focus before the dialog opened, so it can be given back. */
  let opener: Element | null = null;

  $effect(() => {
    opener = document.activeElement;
    panel?.focus();
    return () => {
      if (opener instanceof HTMLElement) opener.focus();
    };
  });

  /**
   * Focus trap. The old inline confirm strip achieved the same thing by
   * putting `disabled={confirmingDelete}` on every other button on the page;
   * a trap enforces it structurally instead, so a control added later cannot
   * forget to opt in.
   */
  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
      return;
    }
    if (e.key !== 'Tab' || !panel) return;
    const focusable = panel.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }
</script>

<svelte:window {onkeydown} />

<div class="scrim">
  <div
    bind:this={panel}
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1">
    <h2>{title}</h2>
    <div class="body">{@render children()}</div>
    <div class="actions">{@render actions()}</div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.55);
    animation: fade var(--t-slow) var(--ease);
  }
  .panel {
    width: min(420px, calc(100vw - 48px));
    padding: 18px 20px 16px;
    background: var(--overlay);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
    animation: rise var(--t-slow) var(--ease);
  }
  .panel:focus-visible { outline: none; }
  h2 { margin: 0 0 8px; font-size: 15px; font-weight: 650; }
  .body { color: var(--dim); font-size: 12.5px; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 18px; }
  @keyframes fade { from { opacity: 0; } }
  @keyframes rise { from { opacity: 0; transform: translateY(8px); } }
</style>
