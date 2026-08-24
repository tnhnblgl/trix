<script lang="ts">
  let {
    combo,
    capturing,
    oncapture,
    onstart,
  }: {
    /** The combination to show, e.g. `alt+f10`. */
    combo: string;
    capturing: boolean;
    oncapture: (e: KeyboardEvent) => void;
    onstart: () => void;
  } = $props();

  // `alt+f10` -> ['Alt', 'F10']. Single characters upper-case ('e' -> 'E');
  // named keys title-case ('f10' -> 'F10', 'ctrl' -> 'Ctrl').
  const caps = $derived(
    combo
      .split('+')
      .filter((p) => p.length > 0)
      .map((p) => (p.length === 1 ? p.toUpperCase() : p[0].toUpperCase() + p.slice(1))),
  );
</script>

<button
  type="button"
  class="hk"
  class:capturing
  aria-label="Clip hotkey"
  onclick={onstart}
  onkeydown={(e) => { if (capturing) oncapture(e); }}>
  {#if caps.length === 0}
    <span class="ask">click, then press a combination</span>
  {:else}
    {#each caps as cap, i (i)}
      {#if i > 0}<span class="plus">+</span>{/if}
      <kbd>{cap}</kbd>
    {/each}
  {/if}
</button>

<style>
  .hk {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 172px;
    padding: 6px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .hk.capturing { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  kbd {
    padding: 2px 7px;
    border-radius: var(--r-sm);
    background: var(--raised);
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    font: inherit;
    font-size: 11px;
    font-weight: 600;
  }
  .plus { color: var(--faint); font-size: 11px; }
  .ask { color: var(--faint); font-size: 12px; }
</style>
