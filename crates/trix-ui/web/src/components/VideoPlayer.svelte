<script lang="ts">
  import type { Snippet } from 'svelte';
  import { formatClock } from '../lib/timeline';
  import {
    VOLUME_STEP, VOLUME_STORAGE_KEY, nudgeVolume, parseStoredVolume, unmutedLevel, volumeIcon,
  } from '../lib/volume';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';
  import Slider from './ui/Slider.svelte';

  let {
    src,
    ontime,
    timeline,
  }: {
    src: string;
    ontime: (ms: number) => void;
    /**
     * The unified band, rendered inside the transport row. Handed the
     * duration this player read off the file -- which is the only duration
     * available for a clip the library recorded as `duration_ms: 0`, and is
     * 0 itself until `loadedmetadata` fires.
     */
    timeline: Snippet<[number]>;
  } = $props();

  let video = $state<HTMLVideoElement | null>(null);
  /** The wrapper, not the video: fullscreen on the `<video>` alone would take
      the transport row -- which is its sibling -- off screen. */
  let stage = $state<HTMLDivElement | null>(null);
  let playing = $state(false);
  let muted = $state(false);
  let full = $state(false);

  // Volume. The level is remembered across launches; muting is not, so a
  // clip never opens silent because of something done in an earlier session.
  const storedLevel = readStoredLevel();
  let level = $state(storedLevel);
  /** Where unmuting a level dragged to zero comes back to. */
  let lastAudible = storedLevel > 0 ? storedLevel : 100;
  let volEl = $state<HTMLDivElement | null>(null);

  function readStoredLevel(): number {
    try {
      return parseStoredVolume(localStorage.getItem(VOLUME_STORAGE_KEY));
    } catch {
      return 100;
    }
  }

  // `volume` and `muted` belong to the element and survive `ClipPage`
  // repointing `src`, but they are set from state here all the same, so the
  // speaker icon and what comes out of the speakers cannot disagree.
  $effect(() => {
    if (!video) return;
    video.volume = level / 100;
    video.muted = muted;
  });

  $effect(() => {
    const value = String(level);
    try {
      localStorage.setItem(VOLUME_STORAGE_KEY, value);
    } catch {
      // Storage refused (a blocked profile): the level lasts this session only.
    }
  });

  function setLevel(v: number) {
    level = v;
    if (v > 0) {
      lastAudible = v;
      muted = false;
    }
  }

  function toggleMute() {
    if (muted || level === 0) {
      setLevel(unmutedLevel(level, lastAudible));
      muted = false;
    } else {
      muted = true;
    }
  }

  // The popup above the speaker. Open while the pointer is over the speaker or
  // the popup, while keyboard focus is in either, and for a moment after a
  // key or wheel step so the change can be seen. Not on mouse focus: WebView2
  // focuses a button when it is clicked, and a popup that stayed up after
  // clicking mute and moving away would read as stuck.
  let hovering = $state(false);
  let keyboardFocus = $state(false);
  let flashing = $state(false);
  const volumeOpen = $derived(hovering || keyboardFocus || flashing);
  let leaveTimer: ReturnType<typeof setTimeout> | undefined;
  let flashTimer: ReturnType<typeof setTimeout> | undefined;

  function onVolEnter() {
    clearTimeout(leaveTimer);
    hovering = true;
  }

  function onVolLeave() {
    // A short grace period, so crossing from the speaker up to the popup --
    // or overshooting its edge mid-adjustment -- does not close it.
    clearTimeout(leaveTimer);
    leaveTimer = setTimeout(() => (hovering = false), 250);
  }

  function onVolFocusIn(e: FocusEvent) {
    const el = e.target as { matches?: (selector: string) => boolean } | null;
    keyboardFocus = typeof el?.matches === 'function' && el.matches(':focus-visible');
  }

  function onVolFocusOut(e: FocusEvent) {
    if (!(e.relatedTarget instanceof Node && volEl?.contains(e.relatedTarget))) keyboardFocus = false;
  }

  /** One volume step up (1) or down (-1), shown briefly in the popup. */
  export function stepVolume(direction: 1 | -1) {
    if (direction === 1) muted = false;
    setLevel(nudgeVolume(level, direction));
    flashing = true;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (flashing = false), 1000);
  }

  // The wheel over the speaker or its popup steps the volume. Registered by
  // hand because it has to call `preventDefault` -- the clip page scrolls --
  // and Svelte attaches `onwheel` as a passive listener, which cannot.
  $effect(() => {
    if (!volEl) return;
    const el = volEl;
    const onwheel = (e: WheelEvent) => {
      if (e.deltaY === 0) return;
      e.preventDefault();
      stepVolume(e.deltaY < 0 ? 1 : -1);
    };
    el.addEventListener('wheel', onwheel, { passive: false });
    return () => el.removeEventListener('wheel', onwheel);
  });

  $effect(() => () => {
    clearTimeout(leaveTimer);
    clearTimeout(flashTimer);
  });
  let positionMs = $state(0);
  let durationMs = $state(0);

  // Cleared when the source changes, because nothing else does it. `ClipPage`
  // repoints `src` on the same instance rather than re-keying the component,
  // and `onloadedmetadata` below deliberately skips the assignment for a
  // non-finite duration -- so an unfinalized or fragmented mp4 stepped to from
  // a 60s clip kept reading `/ 1:00.0` and scaled the seek band to 60000ms.
  // Zero is the honest value: it is what the `duration_ms: 0` clip this guard
  // exists for reports, and `Timeline` already draws a band at that duration.
  $effect(() => {
    void src;
    durationMs = 0;
    positionMs = 0;
  });

  export function toggle() {
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }

  export function seekTo(ms: number) {
    if (video) video.currentTime = ms / 1000;
  }

  $effect(() => {
    const onchange = () => (full = document.fullscreenElement === stage);
    document.addEventListener('fullscreenchange', onchange);
    return () => document.removeEventListener('fullscreenchange', onchange);
  });

  function toggleFull() {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void stage?.requestFullscreen();
  }
</script>

<div bind:this={stage} class="stage">
  <!-- svelte-ignore a11y_media_has_caption -->
  <video
    bind:this={video}
    {src}
    autoplay
    onplay={() => (playing = true)}
    onpause={() => (playing = false)}
    onloadedmetadata={() => {
      // Guarded, not just truthy-checked: an unfinalized or fragmented mp4
      // reports `Infinity`, which sails past every `durationMs <= 0` guard in
      // timeline.ts and lands in `video.currentTime`, whose WebIDL setter
      // rejects a non-finite double. That file is exactly the `duration_ms: 0`
      // clip the seek-only band exists for.
      if (video && Number.isFinite(video.duration)) durationMs = video.duration * 1000;
    }}
    ontimeupdate={() => {
      if (!video) return;
      positionMs = video.currentTime * 1000;
      ontime(positionMs);
    }}
    onclick={toggle}></video>

  <div class="transport">
    <button type="button" class="play" aria-label={playing ? 'Pause' : 'Play'} onclick={toggle}>
      <Icon name={playing ? 'pause' : 'play'} size={13} />
    </button>

    <div class="tl">{@render timeline(durationMs)}</div>

    <span class="clock tnum">{formatClock(positionMs)} / {formatClock(durationMs)}</span>

    <!-- The popup comes after the speaker in the markup so Tab reaches the
         speaker first and then the slider, though it is drawn above. -->
    <div
      bind:this={volEl}
      class="vol"
      role="group"
      aria-label="Volume"
      onpointerenter={onVolEnter}
      onpointerleave={onVolLeave}
      onfocusin={onVolFocusIn}
      onfocusout={onVolFocusOut}>
      <IconButton
        icon={volumeIcon(level, muted)}
        label={muted || level === 0 ? 'Unmute' : 'Mute'}
        onclick={toggleMute} />
      {#if volumeOpen}
        <div class="pop">
          <div class="panel">
            <span class="pct tnum">{muted ? 0 : level}</span>
            <Slider
              vertical
              label="Volume"
              min={0}
              max={100}
              step={VOLUME_STEP}
              value={muted ? 0 : level}
              oninput={setLevel}
              onchange={setLevel} />
          </div>
        </div>
      {/if}
    </div>
    <IconButton
      icon={full ? 'fullscreen-exit' : 'fullscreen'}
      label={full ? 'Leave fullscreen' : 'Fullscreen'}
      onclick={toggleFull} />
  </div>
</div>

<style>
  .stage { display: grid; gap: 12px; }
  .stage:fullscreen { background: var(--video-bg); align-content: center; padding: 0 24px 24px; }
  video {
    width: 100%;
    max-height: 58vh;
    background: var(--video-bg);
    border-radius: var(--r-lg);
    display: block;
    cursor: pointer;
  }
  .stage:fullscreen video { max-height: calc(100vh - 100px); }
  .transport { display: flex; align-items: center; gap: 13px; }
  .play {
    width: 36px;
    height: 36px;
    flex: 0 0 36px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-ink);
    cursor: pointer;
    transition: background var(--t-fast) var(--ease);
  }
  .play:hover { background: var(--accent-hi); }
  .tl { flex: 1; min-width: 0; }
  .clock { font-size: 11px; color: var(--faint); flex: 0 0 auto; }

  /* Above the speaker, over the bottom of the video, so opening it moves
     nothing: the transport row shares its width with the timeline, and a
     slider sliding out sideways would shift the trim handles under the
     pointer. No ancestor up to `.content` sets a transform, filter or
     `will-change`, so this z-index is ranked against the page rather than
     trapped in a stacking context. The padding is the bridge the pointer
     crosses from the speaker, so the gap does not count as leaving. */
  .vol { position: relative; display: flex; }
  .pop {
    position: absolute;
    bottom: 100%;
    left: 50%;
    transform: translateX(-50%);
    padding-bottom: 6px;
    z-index: 10;
  }
  .panel {
    display: grid;
    justify-items: center;
    gap: 8px;
    padding: 9px 6px 12px;
    background: var(--overlay);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md);
    box-shadow: var(--shadow);
  }
  .pct { font-size: 10.5px; color: var(--dim); }
</style>
