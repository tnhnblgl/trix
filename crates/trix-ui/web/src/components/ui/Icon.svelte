<script lang="ts">
  import { ICONS, type IconName } from '../../lib/icons';

  /**
   * Every icon in this app is decorative: something outside it always carries
   * the name. Either it sits beside a text label, or it sits inside an
   * element that is named itself -- a control with an `aria-label`, or the
   * `role="img"` span `ClipCard` wraps its favourite marker in, which is the
   * one place an icon is the whole of the information. So this is
   * `aria-hidden` unconditionally and takes no title -- an icon that
   * announced itself would double up every button in the app.
   */
  let { name, size = 16 }: { name: IconName; size?: number } = $props();

  const def = $derived(ICONS[name]);
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill={def.filled ? 'currentColor' : 'none'}
  stroke={def.filled ? 'none' : 'currentColor'}
  stroke-width="1.35"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false">
  <path d={def.d} />
</svg>

<style>
  svg { display: block; flex: 0 0 auto; }
</style>
