<script lang="ts">
  import { formatBytes, formatDuration, thumbUrl } from '../lib/clips';
  import type { ClipMeta } from '../lib/types';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';
  import Menu from './ui/Menu.svelte';

  let {
    clip, clipDir, selected = false, onopen, onselect, onfavorite, onrename, ondelete,
  }: {
    clip: ClipMeta;
    clipDir: string;
    selected?: boolean;
    onopen: () => void;
    onselect: () => void;
    onfavorite: () => void;
    onrename: (title: string) => void;
    ondelete: () => void;
  } = $props();

  let menuOpen = $state(false);
  let renaming = $state(false);
  let draft = $state('');

  function pick(id: string) {
    menuOpen = false;
    if (id === 'favorite') onfavorite();
    else if (id === 'rename') {
      draft = clip.title;
      renaming = true;
    } else if (id === 'delete') ondelete();
  }

  function commit() {
    if (!renaming) return;
    renaming = false;
    const next = draft.trim();
    if (next && next !== clip.title) onrename(next);
  }
</script>

<div class="card" class:selected class:menuOpen>
  <!-- `aria-label`, because nothing inside this button can name it: the
       thumbnail is `alt=""` (it is the clip, not a description of it) and the
       only other child is the duration badge, so without this the card
       announced itself as "0:12, button". -->
  <button type="button" class="thumb" aria-label={clip.title} onclick={onselect} ondblclick={onopen}>
    <img src={thumbUrl(clipDir, clip.id)} alt="" loading="lazy" />
    <span class="len tnum">{formatDuration(clip.duration_ms)}</span>
  </button>

  <div class="meta">
    <div class="line">
      <!-- The star lives here, not on the thumbnail. Over a bright frame a
           bare glyph is invisible; on the app's own surface it never is. It
           also sits on the same line as the menu item that toggles it.

           `role="img"` with a name, because this is the only place kept state
           is expressed on a card -- the overflow menu that toggles it is not
           read until it is opened, so a colour was the whole of it. The Icon
           inside stays `aria-hidden`; the span is what carries the name. -->
      {#if clip.favorite}
        <span class="star" role="img" aria-label="Favourited"><Icon name="star-filled" size={13} /></span>
      {/if}

      {#if renaming}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="rn"
          data-card-control
          bind:value={draft}
          autofocus
          onblur={commit}
          onkeydown={(e) => {
            // Stopped as well as handled: `Grid.svelte` listens on
            // `<svelte:window>`, and without this every keystroke typed here
            // would also reach the grid's arrow/Enter/Space handling.
            e.stopPropagation();
            if (e.key === 'Enter') commit();
            else if (e.key === 'Escape') renaming = false;
          }} />
      {:else}
        <span class="title">{clip.title}</span>
        <span class="dots" data-card-control>
          <IconButton icon="dots" label="More actions for {clip.title}" size={14}
            onclick={() => (menuOpen = !menuOpen)} />
          {#if menuOpen}
            <Menu
              items={[
                { id: 'favorite', label: clip.favorite ? 'Unfavourite' : 'Favourite', icon: clip.favorite ? 'star-filled' : 'star' },
                { id: 'rename', label: 'Rename', icon: 'pencil' },
                { id: 'delete', label: 'Delete', icon: 'trash', danger: true, separatorBefore: true },
              ]}
              onpick={pick}
              onclose={() => (menuOpen = false)} />
          {/if}
        </span>
      {/if}
    </div>
    <!-- The separators are markup, not string content. An `&middot;` written
         inside a `{...}` expression renders as the six literal characters
         `&middot;` -- entities are only decoded in markup. -->
    <span class="sub tnum">
      {formatBytes(clip.bytes)} &middot; {clip.width}x{clip.height}{#if clip.fps} &middot; {clip.fps} fps{/if}
    </span>
  </div>
</div>

<style>
  .card { display: grid; gap: 8px; transition: transform var(--t-fast) var(--ease); }
  .card:hover { transform: translateY(-2px); }

  /* The open menu is what needs this, not the card.
     `Menu` is `z-index: 30`, but a z-index only ranks an element inside its
     own stacking context -- and `:hover`'s `transform` above makes the card
     one. So the moment the pointer is on the card the menu stops competing
     with the rest of the grid and can only stack against its own siblings.
     The next card down then paints over it: cards are z-index-0 grid items
     drawn in DOM order, and its `.thumb` is `position: relative` for the
     duration badge, which puts it in the same layer as the transformed card
     above it. Hence a menu that reads correctly until you hover.
     Ranking the whole card is the fix -- the menu's own z-index never could,
     from inside. Grid items take `z-index` with no `position`. */
  .card.menuOpen { z-index: 1; }

  .thumb {
    position: relative;
    aspect-ratio: 16 / 10;
    padding: 0;
    border: 0;
    border-radius: var(--r-md);
    overflow: hidden;
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--line-soft);
    cursor: pointer;
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .thumb img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .card:hover .thumb { box-shadow: inset 0 0 0 1px var(--line-strong); }
  .card.selected .thumb {
    box-shadow: inset 0 0 0 2px var(--accent), 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .len {
    position: absolute;
    right: 6px;
    bottom: 6px;
    padding: 1px 6px;
    border-radius: var(--r-sm);
    background: var(--scrim-strong);
    color: var(--text);
    font-size: 10px;
  }

  .meta { display: grid; gap: 1px; padding: 0 2px 2px; }
  .line { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .star { color: var(--fav); display: flex; flex: 0 0 auto; }
  .title { flex: 1; min-width: 0; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { font-size: 10.5px; color: var(--dim); }

  /* Hidden at rest, shown whenever the card is in play: hovered, focused
     within, selected, or with its own menu open. Never a control the user has
     to guess is there. */
  .dots { position: relative; flex: 0 0 auto; opacity: 0; transition: opacity var(--t-fast) var(--ease); }
  .card:hover .dots,
  .card:focus-within .dots,
  .card.selected .dots,
  .card.menuOpen .dots { opacity: 1; }

  .rn {
    flex: 1;
    min-width: 0;
    padding: 3px 7px;
    border-radius: var(--r-sm);
    border: 1px solid var(--accent);
    background: var(--bg);
    color: var(--text);
    font: inherit;
    font-size: 12.5px;
  }
</style>
