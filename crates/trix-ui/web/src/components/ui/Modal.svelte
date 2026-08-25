<script module lang="ts">
  /** Instance counter -- see `uid` below. */
  let nextModalId = 0;
</script>

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
  /**
   * Whether the press that is about to become a click started on the backdrop.
   * A click fires on the nearest common ancestor of press and release, so a
   * text selection dragged out of the panel and released over the scrim
   * arrives here looking exactly like a backdrop click. The dialog body is one
   * sentence with the clip's name bolded in the middle of it -- the sentence a
   * user is most likely to select before deciding.
   */
  let pressedScrim = false;
  /** Whatever had focus before the dialog opened, so it can be given back. */
  let opener: Element | null = null;
  /** Instance-unique, so `aria-labelledby` names this dialog's own heading. */
  const uid = `modal-${nextModalId++}`;

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
    // `select` and `textarea` are in the list even though today's only
    // consumer is a pair of buttons. Leaving them out does not merely strand
    // focus: the wrap is triggered by comparing `activeElement` against the
    // first and last of *this* list, so focus sitting on an unlisted control
    // matches neither, `preventDefault` never runs, and the browser's own Tab
    // walks straight out of the dialog into the page behind it.
    const focusable = panel.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]),'
        + ' textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
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

<!--
  The keydown handler is bound to the panel, NOT to `<svelte:window>`.

  `ClipPage` and `Grid` each already mount their own `<svelte:window
  onkeydown>`. Two listeners on the *same* window are siblings, and
  `stopPropagation` does not stop a sibling on the same node -- only
  `stopImmediatePropagation` does. So a window-bound modal handler would
  double-fire with the page underneath it, and Escape would both close the
  dialog and run the page's shortcut. Bound to the panel, the event stops
  where it is handled and never reaches window at all -- which is exactly why
  `Menu` binds to its own element too. Focus is trapped inside the panel, so
  there is no keydown outside it to miss.
-->
<!--
  A click on the backdrop closes the dialog: the near-universal dismiss
  gesture, and without it the click was worse than inert. It moved focus off
  the panel to `<body>`, and from there Escape no longer reached the
  panel-bound handler above while `ClipPage` swallowed it with
  `if (confirmingDelete) return;`, and Tab walked out of the dialog into the
  page behind -- where Enter on "Next clip" retargeted the open delete dialog
  at a different clip.

  `e.target === e.currentTarget` is what keeps it to the backdrop: the panel
  is a child of this element, so every click inside the dialog bubbles here
  too and only the ones that landed on the scrim itself count. The press has
  to have landed there as well -- see `pressedScrim` above -- because a click
  fires on the common ancestor of press and release, and a dialog whose body
  is a single selectable sentence should not close because a selection was
  dragged past its edge.

  The ignore is for a pointer-only shortcut to an action that already has a
  key. Escape on the panel closes the dialog, and the effect above focuses
  the panel when the dialog opens -- so the keyboard reaches this action
  without the backdrop, and the backdrop needs neither a key handler nor a
  role of its own.
-->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="scrim"
  onpointerdown={(e) => { pressedScrim = e.target === e.currentTarget; }}
  onclick={(e) => { if (pressedScrim && e.target === e.currentTarget) onclose(); }}>
  <div
    bind:this={panel}
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-labelledby="{uid}-title"
    tabindex="-1"
    {onkeydown}>
    <h2 id="{uid}-title">{title}</h2>
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
    background: var(--scrim);
    animation: fade var(--t-slow) var(--ease);
  }
  .panel {
    width: min(420px, calc(100vw - 48px));
    padding: 18px 20px 16px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
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
