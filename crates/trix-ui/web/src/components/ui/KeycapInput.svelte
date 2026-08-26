<script lang="ts">
  import { formatCombo } from '../../lib/ui';

  let {
    combo,
    capturing = false,
    label,
    readonly = false,
    oncapture = () => {},
    onstart = () => {},
  }: {
    /** The combination to show, e.g. `alt+f10`. */
    combo: string;
    capturing?: boolean;
    label: string;
    /**
     * Show the combination without offering to change it: a plain box, not a
     * `<button>`.
     *
     * `FirstRun`'s confirm step is the caller. The wizard has no way to
     * rebind -- that is Settings' job, after setup -- so as a button it took
     * focus, lit up on hover, announced itself as "Clip hotkey, button" and
     * did nothing at all when pressed. The capture props are unused in this
     * mode, which is why they have defaults.
     */
    readonly?: boolean;
    oncapture?: (e: KeyboardEvent) => void;
    onstart?: () => void;
  } = $props();

  const caps = $derived(formatCombo(combo));
</script>

<!-- One rendering of the keycaps for both shells, so a readonly box and the
     capture button can never drift apart. -->
{#snippet keycaps()}
  {#if caps.length === 0}
    <span class="ask">{readonly ? 'none set' : 'click, then press a combination'}</span>
  {:else}
    {#each caps as cap, i (i)}
      {#if i > 0}<span class="plus">+</span>{/if}
      <kbd>{cap}</kbd>
    {/each}
  {/if}
{/snippet}

{#if readonly}
  <!-- `role="group"` with the label, because the caps read as bare letters
       otherwise and there is no control here to carry the name. -->
  <div class="hk ro" role="group" aria-label={label}>{@render keycaps()}</div>
{:else}
  <button
    type="button"
    class="hk"
    class:capturing
    aria-label={label}
    onclick={onstart}
    onkeydown={(e) => { if (capturing) oncapture(e); }}>{@render keycaps()}</button>
{/if}

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
  /* Nothing to press, so nothing that says press me. */
  .hk.ro { cursor: default; }
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
