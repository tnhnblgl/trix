# Trix screenshots — design

**Date:** 2026-08-27
**Status:** approved, ready for planning
**Branch:** `feat/screenshots`

## Goal

A second hotkey takes a still image of what is on screen, saves it next to the
clips, and puts it on the clipboard so it can be pasted straight into Discord.
A new **Screenshots** tab in the desktop app shows them as a grid, opens one
full size, copies it again, deletes it, or reveals it in Explorer.

## The one constraint everything else follows from

**A screenshot comes off the live capture session, so it works only while Trix
is armed.**

This was decided rather than assumed. The alternative — a one-shot grab that
works with capture off — needs a second, independent capture implementation,
because the cheap version of it (a GDI `BitBlt` of the desktop) is exactly the
one that fails on the fullscreen games this is for. That is roughly a day of
new code in the most failure-prone part of Windows, to serve the case where the
user is sitting on the desktop and `Win+Shift+S` already works.

Taking the frame from the armed session instead costs almost nothing, is full
monitor resolution, and captures fullscreen games correctly, because it is the
same frame the encoder is already receiving.

The cost is paid at the one moment it is felt: pressing the key while not armed
must say so. `Daemon::clip` already sets that precedent — it answers
`not armed — send arm first` (`crates/trix-daemon/src/state.rs:621`) rather
than failing quietly. The screenshot path answers the same way, and the UI
raises it as a toast.

## What a press does

1. `WM_HOTKEY` for the screenshot id arrives in `wnd_proc`
   (`crates/trix-daemon/src/window.rs:350` is the clip equivalent).
2. The daemon asks the engine for a frame.
3. The capture callback stages the **next** frame it receives, in BGRA, exactly
   as it already does for clip thumbnails (`crates/trix-core/src/replay.rs:353`
   `request_thumbnail`, `:372` `stage_thumbnail`, `:530` the frame-path hook).
   The wait for it is `THUMB_STAGE_WAIT` (`replay.rs:63`), the same 250 ms
   budget a clip's thumbnail already uses, for the same reason: long enough to
   cover a slow frame, short enough that a frozen screen does not hang the
   request.
4. That buffer is encoded twice: once at full resolution as `{id}.jpg`, once at
   640 px as `{id}.thumb.jpg`.
5. The same buffer is put on the clipboard as a device-independent bitmap.
6. A short chime plays — its own, not the clip chime.
7. `shot_saved` is broadcast, and the Screenshots tab prepends the new tile.

The image is the frame **after** the press, not the frame at the press, so it
lands about one frame late — 16 ms at 60 fps. This is not new: clip thumbnails
have always been taken this way, and `replay.rs:136` documents the trade
already. It is invisible in practice, and it is the reason no frame has to be
retained speculatively.

## Where screenshots live

```
<clip_dir>\Screenshots\
    20260827_143012.jpg          full resolution, quality 0.92
    20260827_143012.thumb.jpg    640 px, for the grid
```

**One folder setting, not two.** Screenshots follow `clip_dir`, so moving the
clips folder moves them, and the folder picker that already exists needs no
sibling.

**Ids are the clip id scheme** — `YYYYMMDD_HHMMSS` with an optional `_N`
collision suffix, validated by the existing `library::is_valid_id`
(`crates/trix-core/src/library.rs:40`) before any id from the socket is
concatenated into a path. Allocation reuses the `create_new` reservation that
`allocate_clip_id` (`:110`) documents at length: the id is claimed atomically,
so two screenshots taken in the same second cannot both believe they own it.

**A subfolder, not the clip folder itself.** `library::thumb_path` is already
`{clip_dir}\{id}.jpg` (`library.rs:29`) — a screenshot written beside the clips
under the same id would overwrite a clip's thumbnail. The subfolder removes the
collision structurally rather than by picking a different suffix and hoping.

The clip library is unaffected by the new folder: `library::scan`
(`library.rs:202`) reads one directory without recursing and skips everything
that is not a `.mp4`.

### No sidecar

Clips carry a `.json` sidecar because a clip has facts that cannot be recovered
from the file cheaply — duration, encoder, whether there is audio. A screenshot
has none of those. The filename carries the timestamp, the directory entry
carries the size, and the JPEG carries its own dimensions.

So screenshots have **no metadata file**, and therefore no metadata that can
drift out of sync with the image, no half-written sidecar to repair, and no
second file to delete.

`created` is derived from the id at scan time by reusing `library.rs`'s
existing `created_from_id`, which yields `2026-08-27T14:30:12` and **claims no
UTC offset**. That is the honest answer for a stamp recovered from a filename:
the offset in force when the shot was taken is not recoverable at scan time,
and stamping today's offset onto a screenshot taken the other side of a DST
change would be wrong twice a year. `library.rs` already made this exact call
for adopted clips, and reusing its function keeps one derivation rather than
two. `Date` in the front end parses an offsetless stamp as local time, which is
the correct reading for a file this machine wrote.

Dimensions are read from the JPEG's `SOF` marker — about twenty lines, and a
pure function that tests directly.

### Why a thumbnail file exists

Because the grid would otherwise decode the full-resolution images. Fifty
1920x1200 JPEGs at ~400 KB decode to roughly 450 MB of bitmap in the webview.
`thumb.rs:29` already carries this exact warning for the clip grid, measured on
this project's target hardware, and the answer there was a 640 px thumbnail.
The answer here is the same one.

Encoding the second image costs about 5 ms and reuses `thumb::encode_jpeg`
unchanged; only its `MAX_WIDTH` needs to become a parameter, so the full-size
call can opt out of downscaling.

## Format

**JPEG, quality 0.92.** About 400 KB for a 1080p frame, ~15 ms to encode, and
it reuses the WIC encoder `thumb.rs` already contains — a different container
GUID is the only thing PNG would have needed, but PNG is 3–5 MB and 100–200 ms
on the low-end CPUs this project targets, which is a hitch mid-game and roughly
ten times the disk.

The visible cost is faint ringing on sharp HUD text under magnification. That
is the right trade for a tool whose pitch is "without thinking FPS and memory".

## The clipboard

Windows will not accept JPEG bytes as an image. The clipboard wants a
device-independent bitmap, and the BGRA buffer already staged is one row-order
flip away from being exactly that. A new `crates/trix-core/src/clipboard.rs`
builds a `CF_DIBV5` and sets it — around 80 lines, and the header construction
is a pure function that tests without touching the clipboard at all.

**A failed copy never costs the file.** The image is written first; the
clipboard is attempted after, and its failure is a `tracing::warn!`. This is the
posture `replay.rs:647` already takes for thumbnails — "a thumbnail is never
allowed to cost the clip" — applied to the same kind of nicety.

`OpenClipboard` genuinely fails in normal use, because another process can hold
it. One retry after a short pause, then give up.

## The sound

Its own chime, not the clip chime, so a clip and a screenshot are
distinguishable by ear mid-game — which is the only moment either sound
matters.

`crates/trix-daemon/assets/shot.wav` is generated by a committed recipe, the
way `clip.wav` is: `crates/trix-core/examples/make-chime.rs` exists so the
sound in the repository has a provenance rather than being an unexplained
binary. A sibling example generates the screenshot sound — shorter and higher
than the clip chime, so the two are told apart instantly rather than compared.

`sound::play` (`crates/trix-daemon/src/sound.rs:32`) takes the built-in or a
custom path today. It gains the ability to name which built-in it wants.

**No custom-sound setting for screenshots.** The clip sound has one because the
clip chime is the product's signature moment. A second file picker, a second
cache, and a second repair path is a large amount of surface for a sound that
plays a few times a session. `screenshot_sound` is a plain on/off.

## Protocol

New commands, in the shape of the `library.*` family, parsed in
`crates/trix-proto/src/command.rs`:

| Command | Arguments | Answers |
|---|---|---|
| `screenshot` | — | `ShotMeta` |
| `shots.list` | `offset`, `limit` | `{ shots, total, offset }` |
| `shots.delete` | `shot_id` | `{}` |
| `shots.reveal` | `shot_id` | `{}` |
| `shots.copy` | `shot_id` | `{}` |

New event `shot_saved`, carrying a `ShotMeta`, broadcast from the single place
that records a saved screenshot — the lesson `state.rs:600` records at length
about `clip_saved`, where the hotkey path went three plans without an event
because two call sites each had to remember to emit one.

```rust
pub struct ShotMeta {
    pub id: String,
    pub created: String,   // "2026-08-27T14:30:12", no offset claimed
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}
```

`screenshot` is a command rather than hotkey-only wiring, so that the tray, the
app, and any third-party client reach it the same way — spec §3.2's rule that
the UI holds no privilege the socket does not.

`shots.copy` exists because the tab needs to put an already-saved screenshot on
the clipboard, and the clipboard is the daemon's to touch: `trix-ui.exe` is not
resident, so a copy implemented in the app would be unavailable in exactly the
situation the tab is open to serve.

## Configuration

Two new keys in `crates/trix-core/src/config.rs`:

```rust
/// Hotkey for a screenshot, same grammar as `clip_hotkey`.
pub screenshot_hotkey: String,     // default "alt+f8"
/// Whether a saved screenshot plays its chime.
pub screenshot_sound: bool,        // default true
```

**`alt+f8` is chosen to be free.** NVIDIA's overlay owns `alt+f9` (record),
`alt+f10` (save replay) and `alt+f1` (screenshot); Steam owns `f12`. This
project already has an NVIDIA App collision on record for `alt+f10`, and a
default that lands on another overlay's key registers as a failure the user
cannot currently see.

Both keys are settings rows in the existing **Trix** section:
`screenshot_hotkey` renders with the same `hotkey` field kind `clip_hotkey`
uses, `screenshot_sound` as a `bool`. `crates/trix-ui/web/src/lib/settings.ts`
grows two `FIELDS` entries and its test grows two shipped keys — nothing in
`Settings.svelte` changes, because it iterates `FIELDS` and both kinds already
render.

## The second hotkey

`window.rs` registers one hotkey under `HOTKEY_ID = 1` (`:126`), at startup
(`:548`) and on rebind (`:384`). A second constant, `SHOT_HOTKEY_ID = 2`, joins
it beside the first, and both sites handle both keys.

`Action` (`window.rs:62`) gains a `Screenshot` variant beside `Clip`, and the
`WM_HOTKEY` arm dispatches on the id it was given rather than assuming.
`rebind_hotkey` (`:243`) takes which key it is rebinding.

**Registration failure stays as it is today: one `tracing::warn!` and nothing
else.** `docs/notes/2026-08-26-hotkey-capture-modes.md` argues that this
silence is the larger defect and that making it visible should come first; that
work is explicitly **out of scope here** and remains its own undecided
decision. This spec is not permitted to make the situation worse — the new key
warns exactly as the old one does — but it does not fix it either.

## The Screenshots tab

`View` (`crates/trix-ui/web/src/lib/state.svelte.ts:7`) gains `'shots'`, the
rail gains a third entry between Clips and Settings, and `App.svelte` gains one
branch.

The grid is deliberately **lean**: view full size, copy, delete, show in
folder. No rename and no favourite — a screenshot's name is its timestamp, and
nobody renames screenshots. Nothing about this blocks adding either later.

- **Grid** — `views/Shots.svelte`, tiles built from the thumbnails, newest
  first. Arrow-key navigation and Delete reuse `lib/keys.ts`, which the clip
  grid already shares.
- **Full size** — clicking a tile opens the image over the grid. Escape closes
  it; arrows move through the set. This is a viewer, not a page: there is
  nothing to trim, so the clip page's structure would be mostly empty chrome.
- **Copy** — `shots.copy`, with a toast on success, because a clipboard write
  is otherwise entirely invisible.
- **Delete** — `shots.delete`, removing both the image and its thumbnail.
- **Show in folder** — `shots.reveal`, through the same mechanism
  `library.reveal` uses.

The empty state names the hotkey, through `formatCombo`, the way `Grid.svelte`
already does for the clip key — that page once printed the raw config string
while Settings two clicks away rendered keycaps for the same setting.

### The asset-scope gotcha

`grant_clip_dir` (`crates/trix-ui/src/daemon.rs:204`) calls
`allow_directory(dir, false)` — **non-recursive**. `Screenshots\` is outside
that grant, and the symptom is every thumbnail in the new tab rendering as a
broken image with nothing logged and nothing on screen to explain it.

A second `allow_directory` for the subfolder goes in that same function, and
for the reason its doc comment already gives: it is the one place both the
app-initiated grant and the event-driven one pass through, so the two cannot
drift apart.

## The library size limit does not apply

`max_library_gb` deletes the oldest non-favourite clips. Screenshots have no
favourite, so a ceiling that swept them would delete files the user has no way
to protect. They are excluded.

At roughly 400 KB each that is about 2,500 screenshots per gigabyte, and the
ceiling ships off by default. If this proves wrong, the fix is a favourite flag
on screenshots first, then inclusion — not inclusion on its own.

## Error handling

| Failure | Result |
|---|---|
| Not armed | `not armed — arm Trix to take a screenshot`, toast in the app |
| No frame staged within 250 ms | Error answer; nothing written |
| Full-size encode fails | Error answer; nothing written; no partial file left |
| Thumbnail encode fails | Screenshot is kept; tile falls back to the full image |
| Clipboard busy or refused | Screenshot is kept; `tracing::warn!` only |
| Sound fails | Ignored, as the clip chime's failures already are |
| Screenshots folder not writable | Error answer naming the folder |
| Hotkey registration fails | One `tracing::warn!` — unchanged, and out of scope |

The rule throughout: **the image is the product.** Everything after the write
is a nicety, and none of them may cost the file.

## Testing

Unit tests, no hardware:

- JPEG dimension parsing from an `SOF` marker, including a truncated file
- `created` derived from an id, and every invalid id rejected
- Id allocation collides into `_2`, `_3`, … under a real temp directory
- Scanning a folder lists images and **never** lists `*.thumb.jpg`
- Deletion removes both files, and tolerates a missing thumbnail
- `CF_DIBV5` header construction against a known BGRA buffer
- `ShotMeta` and every `shots.*` command round-tripping through the protocol
- `Command::parse` rejecting a `shots.*` call with a missing or non-string id
- Front end: URL building for image and thumbnail, including the trailing-slash
  case `clips.ts` documents; `settings.ts` covering both new keys

Not covered, and deliberately: setting the real clipboard, registering a real
hotkey, and staging a real frame. These are the boundaries this project already
draws — they need a capture session and a desktop, and they are verified by
hand at the gate.

**Tests must never touch the developer's real config or clip library.** Every
test above uses a scratch directory, in keeping with the standing rule.

## Out of scope

- **Screenshots while not armed.** Decided against, above.
- **Making hotkey-registration failure visible.** Its own note, its own
  decision.
- **Rename, favourite, search, multi-select** in the tab.
- **PNG, or a format setting.**
- **A custom screenshot sound file.**
- **Annotation, cropping, or any editing.**
- **Region or window capture.** The screenshot is the captured monitor, which
  is the monitor `monitor_index` already selects.
- **Uploading or sharing.** Trix talks to no server, and this does not change
  that: the clipboard is a local paste target, not a network call.
