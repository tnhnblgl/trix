<script lang="ts">
  let {
    checked,
    disabled = false,
    label,
    onchange,
  }: {
    checked: boolean;
    disabled?: boolean;
    label: string;
    onchange: (next: boolean) => void;
  } = $props();
</script>

<button
  type="button"
  class="tg"
  class:on={checked}
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}>
  <span class="knob"></span>
</button>

<style>
  .tg {
    position: relative;
    width: 38px;
    height: 21px;
    flex: 0 0 auto;
    padding: 0;
    border-radius: var(--r-full);
    border: 1px solid var(--line-strong);
    background: var(--line-strong);
    cursor: pointer;
    transition: background var(--t) var(--ease), border-color var(--t) var(--ease);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--dim);
    transition: left var(--t) var(--ease), background var(--t) var(--ease);
  }
  .tg.on { background: var(--accent); border-color: var(--accent); }
  .tg.on .knob { left: 20px; background: var(--text); }
  /* Every other interactive control in this folder answers the pointer; this
     one did not, against app.css's own argument that a control which does not
     move under the cursor reads as decoration. Off brightens to --line-hi,
     which is exactly what that token is for; on brightens to --accent-hi, the
     same step `Button.primary` and the play button take.

     Scoped with `:not(.on)` rather than relying on source order: `.tg:hover`
     would otherwise outrank `.tg.on` on specificity and wash the accent out
     from under the pointer -- the same trap `.arm` in TitleBar documents. */
  .tg:hover:not(:disabled):not(.on) { background: var(--line-hi); border-color: var(--line-hi); }
  .tg:hover:not(:disabled):not(.on) .knob { background: var(--text); }
  .tg.on:hover:not(:disabled) { background: var(--accent-hi); border-color: var(--accent-hi); }
  .tg:disabled { opacity: 0.4; cursor: default; }
</style>
