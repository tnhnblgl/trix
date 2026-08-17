# Trix clip sound — design

**Date:** 2026-08-17
**Status:** approved, ready for planning
**Branch:** `feat/clip-sound`

## Goal

When Trix saves a clip, it plays a short sound, so you know the hotkey worked
without alt-tabbing out of the game. The sound can be replaced with your own
`.wav`, and put back with one button.

## Why the daemon plays it, not the UI

The obvious place is the UI — `clip_saved` already reaches it, and the toast it
raises today sits at `crates/trix-ui/web/src/lib/state.svelte.ts:269`. That
would have been a one-file change.

It is the wrong place. `trix-ui.exe` is not resident: the tray icon and the
recording both live in `trix-daemon.exe`, and closing the Trix window exits the
UI process while the daemon carries on. A sound played by the UI is therefore
silent in the one situation the sound exists for — mid-game, window closed.

The daemon is already the process that knows a clip was saved, at the single
broadcast site in `crates/trix-daemon/src/state.rs:629`. It plays the sound.
The UI owns the settings and nothing else, which keeps spec §3.2 intact: the UI
still reaches every capability over the control socket.

## Playback

`PlaySoundW` from `winmm.dll`, bound in the already-pinned `windows 0.62.2` at
`Win32::Media::Audio`. This adds no dependency — only the `Win32_Media_Audio`
feature to the workspace `windows` entry in `Cargo.toml`.

- Built-in sound: `SND_MEMORY | SND_ASYNC | SND_NODEFAULT`
- Custom sound: `SND_FILENAME | SND_ASYNC | SND_NODEFAULT`

`SND_ASYNC` returns immediately, so the clip-save path is never blocked waiting
on audio. `SND_ASYNC` also requires the sound buffer to stay valid until
playback finishes; the built-in sound is a `'static` slice from `include_bytes!`,
which satisfies that by construction rather than by careful lifetime management.

`SND_NODEFAULT` is what stops Windows substituting its own default ding when the
sound cannot be played. A missing or malformed file plays *nothing*.

**Playback failure never affects the clip.** The call is made after the clip is
written and after `clip_saved` is broadcast. A failed `PlaySoundW` is logged at
`warn` and discarded.

### The built-in sound

A short chime, generated once as 16-bit PCM mono WAV, committed to the repo at
`crates/trix-daemon/assets/clip.wav` and embedded with `include_bytes!`. Budget:
under 16 KB.

It is generated rather than sourced so there is no third-party audio licence in
the tree, and embedded rather than shipped as a file so the release zip keeps
its five entries — the updater's `BINARIES`/`DOCS` lists and `SHA256SUMS.txt`
flow stay untouched.

## Configuration

Two keys on `Config` in `crates/trix-core/src/config.rs`:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `clip_sound` | `bool` | `true` | Whether to play a sound when a clip is saved. |
| `clip_sound_path` | `String` | `""` | Path to a custom `.wav`. Empty means the built-in sound. |

`clip_sound` defaults to `true` via the existing `default_true` helper, the same
way `check_for_updates` does.

Both keys become settable with no further work: `set_config`
(`crates/trix-daemon/src/state.rs:758`) reads its known-key set off a serialized
`Config::default()` precisely so that a key added later is settable without
anyone remembering that function exists.

**Empty is the default, and that is the whole of "Reset to default".** There is
no default file on disk to restore and nothing that can go missing.

### Validation

`clip_sound_path` gets a filesystem gate in `set_config`, guarded on the key
being present, as a sibling of the existing `clip_dir` gate at
`crates/trix-daemon/src/state.rs:809`. A non-empty value is refused unless:

1. the path exists and is a file, and
2. its extension is `.wav`, case-insensitively, and
3. its first twelve bytes are a RIFF/WAVE header (`RIFF` at 0, `WAVE` at 8).

The header check is what makes the difference between a rejection the user can
act on and a silent nothing at clip time — `.wav` in the name proves nothing,
and `SND_NODEFAULT` means a mislabelled file simply never plays.

Refusal follows the existing all-or-nothing contract: the caller is told no, the
old value stands, and nothing is written. The UI renders that error inline the
way it does for any other rejected key.

## Choosing a file

A new `sound.pick` command on the control socket opens a Windows file dialog
filtered to `.wav`.

`crates/trix-daemon/src/folder.rs:80` already does exactly this for folders, and
its two hard-won properties carry over unchanged: the dialog runs on its own
thread with its own COM apartment, and it is deliberately **unowned**, because
the daemon's window is `HWND_MESSAGE` and a dialog owned by it inherits a
position in no z-order at all. The file variant is that code with `FOS_PICKFOLDERS`
replaced by `FOS_FILEMUSTEXIST` and a `SetFileTypes` filter.

**`sound.pick` answers `ok` immediately and does not wait for the dialog.** This
is the one place the folder picker's design cannot be copied: `folder::pick`
blocks its caller until the user answers, which is harmless on the tray's pump
thread and unacceptable on a socket command — the daemon would stop recording
for as long as a dialog sat open on screen.

So the picker thread is spawned and detached. When the user chooses a file, that
thread applies it through the normal `config.set` path, which already broadcasts
every accepted key to every connected client
(`crates/trix-daemon/src/dispatch.rs:128`). The open Settings page updates itself
through machinery that already exists. Cancelling changes nothing and broadcasts
nothing. **No new event type is introduced.**

A second `sound.pick` while a dialog is already open answers `ok` and does
nothing, so a double-click cannot stack two dialogs.

Neither `sound.pick` nor `sound.test` needs a new Tauri command: both ride the
existing `trix_call` invoke to the socket, like every other capability the UI
has.

## Testing the sound

A `sound.test` command plays the currently configured sound — built-in or
custom — through the same code path clip-save uses.

This is not decoration. The failure this feature can produce is a custom sound
that is accepted but inaudible: wrong device, silent file, volume at zero. Test
is what makes that discoverable at the moment of choosing rather than the next
time something worth clipping happens. It mirrors the hotkey field's existing
Test button.

## The UI

One new `FieldKind`, `sound`, in `crates/trix-ui/web/src/lib/settings.ts`, and
its rendering in `crates/trix-ui/web/src/components/Field.svelte`.

Two rows in the **Trix** section — app behaviour, beside the hotkey and
autostart. Deliberately *not* the **Audio** section, which is about what goes
into the recording; a notification sound listed beside "Microphone" and "PC
sound" reads as something that ends up in the clip.

```
Clip sound          [x] Play a sound when a clip is saved
                    Plays even when the Trix window is closed.

Sound file          [ C:\Users\...\airhorn.wav        ]  Choose...  Test  Reset
                    Your own .wav, or Trix's built-in sound. Custom sounds
                    must be .wav files.
```

- The path box is read-only. `Choose…` issues `sound.pick`.
- `Reset` sets `clip_sound_path` to `""` and is disabled when it already is.
- `Test` issues `sound.test`.
- When `clip_sound` is off, the whole file row — path, `Choose…`, `Test`,
  `Reset` — is disabled rather than hidden. Hiding it makes the toggle look like
  it removed a setting.
- The path box shows `Trix's built-in sound` when the key is empty, not an empty
  box, so the default state is legible.

The section list stays data-driven: `Settings.svelte` iterates the exported
`SECTIONS` constant, and no literal section list is introduced.

## Error handling summary

| Situation | Behaviour |
| --- | --- |
| Custom file missing, not `.wav`, or not a RIFF/WAVE file | `config.set` refuses it; the UI shows the error inline; the previous sound stays in force |
| Custom file deleted after it was accepted | Nothing plays. Logged at `warn`. The clip is unaffected |
| `PlaySoundW` fails for any other reason | Logged at `warn` and discarded. The clip is unaffected |
| `clip_sound` is `false` | No sound on clip save. The file row is disabled, so `Test` is unreachable from the UI |
| Dialog cancelled | Nothing changes, nothing is broadcast |
| `sound.pick` while a dialog is open | Ignored |

## Testing

**Unit tests (CI):**

- the RIFF/WAVE header check accepts a real header and rejects: a text file
  renamed to `.wav`, a file shorter than twelve bytes, and an empty file
- `clip_sound_path` validation refuses a non-existent path and a non-`.wav`
  extension, and accepts `""`
- `clip_sound` and `clip_sound_path` round-trip through `config.toml` and are
  present in `Config::default()`'s serialization, which is what makes them
  settable
- the embedded asset is a well-formed WAV — the same header check, applied to
  `include_bytes!` at test time, so a corrupted asset fails the build's tests
  rather than shipping silently

Tests that touch config must use a scratch `%APPDATA%` **and** a scratch
`clip_dir`, per the standing rule: an empty `clip_dir` resolves to the real
`%USERPROFILE%\Videos\Trix`.

**Hand verification (cannot be automated):**

- a clip actually makes a sound, with the Trix window closed
- a custom `.wav` is picked, plays, and survives a daemon restart
- Reset returns to the built-in sound
- the dialog appears in front of the Trix window (see Known limits)

`PlaySoundW` itself is not asserted in CI, for the same reason the capture stack
is not: it needs a real audio device.

## Known limits, accepted

**No volume control.** `PlaySound` has none. Windows' per-app volume mixer
already gives the user a slider for `trix-daemon.exe`, which is the same control
one step further away. A volume slider means replacing `PlaySound` with
`waveOut` or XAudio2 and owning mixing and device changes — a large amount of
machinery for a notification sound.

**Custom sounds are `.wav` only.** `PlaySound` decodes nothing else, and the
alternative is an mp3/ogg decoder inside a tool whose entire pitch is being
small. The file dialog says so, and the validation error says so.

**The dialog may open behind the Trix window.** The UI asks the daemon to open
it, and Windows grants foreground rights to the process the user last interacted
with — which is `trix-ui.exe`, not `trix-daemon.exe`. The tray's folder picker
does not have this problem because the tray click is itself the interaction.
This is a real risk, not a theoretical one, and it is left to hand-testing: if
it shows up, the fix is one `AllowSetForegroundWindow(daemon_pid)` call from the
UI before issuing `sound.pick`. Building that in advance would be guessing at
Windows' focus rules rather than observing them.

## Out of scope

- Sounds for any other event (armed, disarmed, error)
- Per-sound volume
- Formats other than `.wav`
- A sound picker in the tray menu
