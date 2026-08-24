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
    background: rgba(255, 255, 255, 0.13);
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
  .tg.on .knob { left: 20px; background: #fff; }
  .tg:disabled { opacity: 0.4; cursor: default; }
</style>
