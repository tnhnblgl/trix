<script module lang="ts">
  /** Instance counter -- see `uid` below, same reasoning as Menu.svelte. */
  let nextSelectId = 0;
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { nextIndex } from '../../lib/ui';

  let {
    value,
    options,
    label,
    onchange,
  }: {
    value: string;
    options: { value: string; label: string }[];
    label: string;
    onchange: (value: string) => void;
  } = $props();

  let open = $state(false);
  let active = $state(0);
  let root = $state<HTMLDivElement | null>(null);
  let trigger = $state<HTMLButtonElement | null>(null);

  /**
   * A prefix unique to this instance, for the option ids `aria-activedescendant`
   * points at. Index-based rather than keyed on `option.value` -- a config
   * value is not guaranteed to be a valid id token, and the index is unique
   * regardless. Module-scoped counter for the same reason as Menu.svelte's
   * `uid`: deterministic, free, and safe if two selects are ever mounted in
   * the same frame.
   */
  const uid = `select-${nextSelectId++}`;

  const selected = $derived(options.find((o) => o.value === value));

  function show() {
    // Open on the current value, not on the top of the list, so the first
    // arrow key moves away from where the user already is.
    active = Math.max(0, options.findIndex((o) => o.value === value));
    open = true;
  }

  /** Close and give focus back to the trigger -- a Tab from a closed
      dropdown must continue from the control, not from the top of the page. */
  function dismiss() {
    open = false;
    trigger?.focus();
  }

  function pick(index: number) {
    const option = options[index];
    // Re-picking the option already showing is not a change, and every
    // consumer's `onchange` here is a daemon round trip that rewrites
    // config.toml. Guarded in the primitive rather than in each caller, so
    // `Stepper` and `Select` agree about what a no-op means.
    if (option && option.value !== value) onchange(option.value);
    dismiss();
  }

  $effect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (root && e.target instanceof Node && !root.contains(e.target)) open = false;
    };
    document.addEventListener('pointerdown', away, true);
    return () => document.removeEventListener('pointerdown', away, true);
  });

  function onkeydown(e: KeyboardEvent) {
    if (!open) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault();
        show();
      }
      return;
    }
    // Stopped as well as prevented: Settings mounts no window-level key
    // handler today, but a dropdown that let Escape through would close
    // itself and navigate on any page that adds one later.
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      dismiss();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      e.stopPropagation();
      active = nextIndex(active, e.key === 'ArrowDown' ? 1 : -1, options.length);
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      e.stopPropagation();
      pick(active);
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      e.stopPropagation();
      active = e.key === 'Home' ? 0 : options.length - 1;
    } else if (e.key === 'Tab') {
      // Tab moves on rather than being trapped, but the list must not be
      // left hanging open over the control the user just moved to.
      open = false;
    }
  }
</script>

<div bind:this={root} class="wrap">
  <button
    bind:this={trigger}
    type="button"
    class="trigger"
    class:open
    role="combobox"
    aria-expanded={open}
    aria-haspopup="listbox"
    aria-controls={open ? `${uid}-listbox` : undefined}
    aria-activedescendant={open && options[active] ? `${uid}-${active}` : undefined}
    aria-label={label}
    onclick={() => (open ? dismiss() : show())}
    {onkeydown}>
    <span class="txt">{selected?.label ?? ''}</span>
    <Icon name="chevron-down" size={11} />
  </button>

  {#if open}
    <div id="{uid}-listbox" class="pop" role="listbox" aria-label={label}>
      {#each options as option, i (option.value)}
        <button
          type="button"
          id="{uid}-{i}"
          class="opt"
          class:active={i === active}
          class:on={option.value === value}
          role="option"
          tabindex="-1"
          aria-selected={option.value === value}
          onpointerenter={() => (active = i)}
          onclick={() => pick(i)}>
          <span class="txt">{option.label}</span>
          {#if option.value === value}<Icon name="check" size={12} />{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap { position: relative; flex: 0 0 auto; }
  .trigger {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    width: 100%;
    min-width: 212px;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
    transition: border-color var(--t-fast) var(--ease);
  }
  .trigger:hover { border-color: var(--line-hi); }
  .trigger.open { border-color: var(--accent); box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent); }
  .trigger :global(svg) { color: var(--faint); }
  .txt { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pop {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 30;
    width: 100%;
    padding: 5px;
    display: grid;
    gap: 1px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 9px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
    text-align: left;
    cursor: pointer;
  }
  .opt .txt { flex: 1; }
  .opt.active { background: var(--hover); }
  .opt.on { color: var(--text); }
  .opt.on :global(svg) { color: var(--accent); }
</style>
