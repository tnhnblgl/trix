<script lang="ts">
  import type { Snippet } from 'svelte';
  import { formatClock } from '../lib/timeline';
  import Icon from './ui/Icon.svelte';
  import IconButton from './ui/IconButton.svelte';

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
  let positionMs = $state(0);
  let durationMs = $state(0);

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
    onloadedmetadata={() => { if (video) durationMs = video.duration * 1000; }}
    ontimeupdate={() => {
      if (!video) return;
      positionMs = video.currentTime * 1000;
      ontime(positionMs);
    }}
    onclick={toggle}></video>

  <div class="transport">
    <button class="play" aria-label={playing ? 'Pause' : 'Play'} onclick={toggle}>
      <Icon name={playing ? 'pause' : 'play'} size={13} />
    </button>

    <div class="tl">{@render timeline(durationMs)}</div>

    <span class="clock tnum">{formatClock(positionMs)} / {formatClock(durationMs)}</span>

    <IconButton
      icon={muted ? 'volume-mute' : 'volume'}
      label={muted ? 'Unmute' : 'Mute'}
      onclick={() => { muted = !muted; if (video) video.muted = muted; }} />
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
</style>
