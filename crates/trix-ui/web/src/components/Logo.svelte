<script lang="ts">
  /**
   * The Trix mark, redrawn as vectors from `assets/logo-transparent.png` so it
   * stays sharp at title-bar size. Traced from edge lines measured off that
   * PNG; rendered back at 1024px it differs from it on 0.8% of the mark's
   * pixels, all of them along anti-aliased edges.
   *
   * One deliberate change: the outer chevron is dark navy in the PNG
   * (#1e1e5a), which all but disappears against this app's near-black
   * surfaces and leaves a cyan T beside half a triangle. Here it takes
   * `currentColor` -- the app's text colour unless a parent says otherwise --
   * the way a logo's version for dark backgrounds swaps its ink. The cyan and
   * the blue are the PNG's own.
   *
   * Decorative: every place it appears has the name written beside it.
   */
  let { height = 20 }: { height?: number } = $props();

  // The mark's own bounds on the 1024 grid, with a few units of margin.
  const VIEW = { x: 264, y: 252, w: 562, h: 494 };
</script>

<svg
  width={(height * VIEW.w) / VIEW.h}
  {height}
  viewBox="{VIEW.x} {VIEW.y} {VIEW.w} {VIEW.h}"
  aria-hidden="true"
  focusable="false">
  <path fill="currentColor"
    d="M397.5 343.5L397.5 297.2A40.7 40.7 0 0 1 458.4 261.9L798.4 456.2A45 45 0 0 1 799.9 533.4L476.1 735A36.4 36.4 0 0 1 420.5 704.1L420.5 699.9L738.9 495.3L482.3 343.5z" />
  <path fill="#095ded" d="M495 404.5L585.4 404.5L738.9 495.3L495 652.1z" />
  <path fill="#0ae7fb"
    d="M269 343.5L482.3 343.5L585.4 404.5L495 404.5L495 652.1L420.5 699.9L420.5 404.5L328 404.5z" />
</svg>

<style>
  svg { display: block; flex: 0 0 auto; color: var(--text); }
</style>
