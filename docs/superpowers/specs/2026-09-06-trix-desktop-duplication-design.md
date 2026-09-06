# Desktop Duplication capture backend — design

**Status:** proposed. Nothing started.
**Date:** 2026-09-06

## Goal

Give Trix a second way to get frames off the screen — DXGI Desktop
Duplication — behind a user-visible **Capture method** setting, so the Windows
capture border can be avoided on machines where WGC cannot suppress it.
Windows Graphics Capture stays the default and the shipped behaviour.

## Why

A user reported a yellow border around the screen while playing Rainbow Six
Siege with Trix armed. It was diagnosed to the end, and the conclusion is that
Trix is not at fault:

| Observation | Source |
| --- | --- |
| Border only with Trix **armed** and R6 **running** — neither alone | user, four-way test |
| Not present in the saved clip | user |
| `IsBorderRequired` supported on their build (23H2 22631.6199) | daemon log |
| Borderless consent **granted** | daemon log, `borderless capture consent granted` |
| Capture session never rebuilt while the border was up | daemon log, no `capture session lost` |
| Medal records R6 with **no** border | user |
| OBS on **WGC** → border. OBS on **DXGI duplication** → no border | user |

So Trix asks for everything the API offers and gets the border anyway, and OBS
reproduces it exactly on the same API. Microsoft documents the mechanism:

> if the `IsBorderRequired` property is set to **true** for the same window or
> display by other apps on the device, the border will be displayed.

Something in the R6 process tree flags the display, the indicator is painted
only while a capture is actually running, and a `false` from Trix cannot
override a `true` from anyone else. There is no fix inside WGC.

**The Windows 10 case.** `IsBorderRequired` does not exist before Windows 11,
so `border_settings()` takes its fallback branch there and Trix has no way to
suppress its own contribution at all — no property, no consent, no workaround.

Checked since: Windows 10 does **not** draw the border every time. That fits
the mechanism rather than contradicting it. The indicator appears when some app
flags the display *and* something is actually capturing; Windows 10 simply
removes Trix's only way out of it when that happens. So Windows 10 users are
not permanently bordered — they are bordered in strictly more situations than
Windows 11 users, with no escape hatch.

Real, but not the emergency an unconditional border would have been. The
phasing below reflects that: this is an escape hatch for users who hit the
border, not a fix being pushed at everyone.

## Non-goals

- Replacing WGC. It stays the default, and stays the better backend where it
  works: it handles hybrid-GPU output routing transparently, it gives us the
  cursor for free, and it has `MinUpdateInterval` frame-rate limiting that
  Desktop Duplication has no equivalent for.
- Window capture. Trix captures a monitor and will continue to.
- Changing the encoder, the ring buffer, the clip format, or anything
  downstream of the frame. This work ends at the texture.
- Automatic fallback between backends at runtime. The setting is explicit.

## Architecture

### The seam

The three capture handlers (`ReplaySession`, `RecordSession`, `ProbeCapture`)
implement `windows-capture`'s `GraphicsCaptureApiHandler` directly today. What
they actually consume from it is much smaller than that trait:

- at construction: an `ID3D11Device` (`ctx.device`) and their own flags
- per frame: an `&ID3D11Texture2D`, a QPC timestamp in 100 ns units, width,
  height
- a way to stop the session from inside the callback (`ProbeCapture`'s
  snapshot mode uses this)
- `on_closed`

That is the whole surface, and it is backend-agnostic. So the seam is a
narrow trait in a new `crates/trix-core/src/capture/source.rs`:

```rust
/// One frame, however it was captured. Borrowed: the texture is only valid
/// for the duration of the call, because Desktop Duplication invalidates it
/// at `ReleaseFrame`.
pub struct SourceFrame<'a> {
    pub texture: &'a ID3D11Texture2D,
    /// QPC, in 100 ns units, on the same clock as the audio mixer's `t0`.
    pub qpc_100ns: i64,
    pub width: u32,
    pub height: u32,
}

pub enum Flow {
    Continue,
    Stop,
}

pub trait FrameSink: Send + Sized + 'static {
    type Flags: Send;
    fn new(device: &ID3D11Device, flags: Self::Flags) -> Result<Self>;
    fn on_frame(&mut self, frame: SourceFrame<'_>) -> Result<Flow>;
    fn on_closed(&mut self) -> Result<()> {
        Ok(())
    }
}
```

and one entry point that picks a backend:

```rust
pub fn start<S: FrameSink>(
    method: CaptureMethod,
    monitor_index: u32,
    fps: u32,
    flags: S::Flags,
) -> Result<CaptureHandle<S>>;
```

`CaptureHandle<S>` mirrors the parts of `windows_capture::CaptureControl` that
`replay.rs` and `record.rs` already use — `callback() -> Arc<Mutex<S>>`,
`stop()`, and the wait/finished check — so the call sites change by a name and
not by a shape.

### Backend 1: WGC (existing behaviour, re-wrapped)

A generic adapter, so no capture logic moves:

```rust
struct WgcAdapter<S: FrameSink>(S);

impl<S: FrameSink> GraphicsCaptureApiHandler for WgcAdapter<S> {
    type Flags = S::Flags;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        Ok(Self(S::new(&ctx.device, ctx.flags)?))
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<()> {
        let qpc_100ns = frame.timestamp()?.Duration;
        let flow = self.0.on_frame(SourceFrame {
            texture: frame.as_raw_texture(),
            qpc_100ns,
            width: frame.width(),
            height: frame.height(),
        })?;
        if matches!(flow, Flow::Stop) {
            control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        self.0.on_closed()
    }
}
```

Converting the three existing handlers to `FrameSink` is mechanical:
`frame.as_raw_texture()` becomes `frame.texture`, `frame.timestamp()?.Duration`
becomes `frame.qpc_100ns`, `capture_control.stop()` becomes
`return Ok(Flow::Stop)`. The `settings()` builders, `border_settings()` and
`min_update_interval()` all stay exactly where they are, used only by this
backend.

### Backend 2: Desktop Duplication (new)

Owns its own thread and its own D3D11 device, and runs an acquire loop:

1. Pick the adapter and output for `monitor_index` (see **Adapter selection**).
2. `D3D11CreateDevice` on that adapter; `IDXGIOutput1::DuplicateOutput`.
3. Loop: `AcquireNextFrame(timeout)` → `QueryInterface` the returned
   `IDXGIResource` to `ID3D11Texture2D` → build a `SourceFrame` → call
   `sink.on_frame(..)` → `ReleaseFrame`.
4. On `DXGI_ERROR_WAIT_TIMEOUT`, no new frame — continue. This is normal: like
   WGC, Desktop Duplication only delivers on change, and a static screen
   delivers nothing.
5. On `DXGI_ERROR_ACCESS_LOST`, the duplication is dead — see **Access loss**.

**The texture must not outlive `ReleaseFrame`.** The sink copies it during
`on_frame` (`VideoConverter::convert` does a GPU blit into its own NV12 pool),
so the borrowed `SourceFrame` lifetime already expresses this correctly and
nothing needs to change in the sinks. This is the single most important
invariant in the backend and must be stated in the module doc.

### Timestamps

Not interchangeable, and getting this wrong desynchronises audio.

- **WGC** gives `SystemRelativeTime`, a `TimeSpan` already in **100 ns units**.
- **Desktop Duplication** gives `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime`, raw
  **QPC ticks**.

The DD backend converts once per frame with a `QueryPerformanceFrequency`
cached at construction:

```rust
qpc_100ns = last_present_time * 10_000_000 / qpf
```

Both clocks are QPC-based, so after conversion they are the same timeline the
audio mixer's `t0` and the encoder's `pts` already use. `LastPresentTime` can
be 0 when only the mouse moved — those frames carry no new desktop content and
are skipped rather than timestamped from the wall clock.

### Cursor

This is the biggest single work item, and the one place the two backends are
not equivalent.

WGC composites the cursor for us (`CursorCaptureSettings::WithCursor`).
Desktop Duplication does not: it delivers the pointer **position** in
`DXGI_OUTDUPL_FRAME_INFO::PointerPosition` and the pointer **shape**
separately via `GetFramePointerShape`, and expects the application to draw it.

Required work:

- Cache the pointer shape; it is only re-sent when it changes
  (`PointerShapeBufferSize > 0`), so a backend that only reads it when present
  will lose the cursor entirely on most frames.
- Handle all three shape types: `MONOCHROME` (1-bpp AND/XOR mask, needs real
  masking), `COLOR` (BGRA, straightforward), `MASKED_COLOR` (per-pixel choice
  between copy and XOR).
- Composite into a copy of the frame, never into the duplication texture —
  writing into the surface DXGI handed us is not ours to do.

`PointerPosition.Visible` is false when another output owns the pointer; skip
compositing then. Monochrome cursors are rare in games but are what Windows
falls back to, and a backend that ignores them shows no cursor exactly when a
user is most likely to file a bug about it.

### Access loss

`AcquireNextFrame` returns `DXGI_ERROR_ACCESS_LOST` on desktop switches, UAC
prompts, mode changes, and fullscreen transitions. This is routine, not
exceptional — a game launching is one of the causes.

Trix already has the right structure for it: `run_driven_inner` in `replay.rs`
is a rebuild loop with a 2 s settle, a failure budget, and a ring that restarts
empty. **Surface access loss as session death and let the existing loop
rebuild**, rather than re-acquiring inside the backend. One recovery path, one
place where the "replay ring restarts empty" warning is emitted, and the
behaviour a user sees is identical to what a display change already does today.

### Adapter selection (hybrid GPU)

The largest technical risk. Desktop Duplication must be created on the adapter
that **drives the output**, not the one rendering the game. On a hybrid-GPU
laptop with the display on the iGPU and the game on the dGPU, duplicating from
the wrong adapter fails with `DXGI_ERROR_UNSUPPORTED`. WGC hides this entirely.

`probe.rs` already walks `CreateDXGIFactory1` → adapters → outputs and already
has a test asserting that its monitor index space agrees with
`windows-capture`'s. The backend must resolve `monitor_index` through that same
enumeration and create its device on the owning adapter — never on the default
adapter.

The developer machine is hybrid GPU, so this is testable here.

### Config and UI

**`config.rs`** — one new key, modelled on `gpu_priority` (a `String` with a
reader that falls back on unknown values, so a hand-edited config never fails
to start):

```rust
/// How frames are taken off the screen. `"auto"` (the default), `"wgc"`
/// (Windows Graphics Capture), or `"dd"` (DXGI Desktop Duplication).
pub capture_method: String,
```

with `pub fn capture_method(&self) -> CaptureMethod` warning and returning
`Auto` on anything unrecognised.

**`state.rs`** — add `capture_method` to `REQUIRES_REARM` (currently 7 entries,
becomes 8). Changing how frames are captured cannot take effect on a live
session, and the UI already knows how to say so.

**`settings.ts`** — one `select` field in the Quality section, following the
existing descriptor pattern:

```ts
{ key: 'capture_method', label: 'Capture method', kind: 'select', section: 'Quality', options: [
    { value: 'auto', label: 'Automatic' },
    { value: 'wgc', label: 'Windows Graphics Capture' },
    { value: 'dd', label: 'Desktop Duplication' },
  ], help: '...' },
```

The help text has to earn its place, because this is the one setting where a
user must self-diagnose. It should name the symptom, not the API: something to
the effect of *"If Windows draws a yellow border around your screen while Trix
is armed, choose Desktop Duplication."*

Add the key to `settings.test.ts`'s shipped-keys list, or `unknownKeys` shows
every user the "this daemon has settings this app does not render yet" banner.

### What "Automatic" means

OBS has already solved this, and its policy is worth adopting rather than
inventing one. `choose_method` in `plugins/win-capture/duplicator-monitor-capture.c`
decides, in order:

1. WGC unsupported on this OS → **Desktop Duplication**.
2. Otherwise start at **Desktop Duplication** — it is OBS's default, which is
   why the reporting user saw Automatic pick DXGI.
3. The monitor is not duplicable from the current adapter → **WGC**.
4. The machine **has a battery** and reports **two or more adapters** → **WGC**.

Step 4 is the important one, and it changes this design. OBS does not solve
cross-adapter duplication on hybrid-GPU laptops — **it avoids Desktop
Duplication there entirely and uses WGC instead.** That is a far cheaper answer
than routing duplication to the correct adapter, it is proven in the field, and
it converts the largest risk in this document into a detection problem:
`GetSystemPowerStatus` for a battery, and an adapter count.

The reporting user's machine is a single-adapter desktop, so steps 3 and 4 both
miss and Automatic lands on Desktop Duplication. That matches what they
observed.

**Licensing.** OBS is GPL-2.0 and Trix is MIT. This policy is adopted as
described behaviour and must be reimplemented from scratch. **No OBS code is to
be copied into this repository**, here or in the cursor work below.

Phasing, given the Windows 10 finding:

**Ship 1 — `auto` = WGC, always.** Desktop Duplication is opt-in only. Nobody's
behaviour changes unless they choose it, and the backend earns trust on real
machines before it is anyone's default.

**Ship 2 — `auto` = the OBS policy above.** Only once the backend has held up
in real use. This is the one that can regress users who have no complaint
today, which is exactly why it does not ship first.

## Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Hybrid-GPU adapter routing | Low, was High | Not solved — avoided. Automatic picks WGC on any battery-powered machine with 2+ adapters, as OBS does. A user who forces Desktop Duplication there gets a clear failure, not a wrong-adapter capture |
| Cursor compositing wrong or missing | High | All three shape types, shape caching; visual check against the WGC backend |
| Exclusive fullscreen behaves differently | Medium | Test R6 in all three display modes; access loss is expected on transitions and the rebuild loop absorbs it |
| No `MinUpdateInterval` equivalent | Medium | The QPC pacer in `on_frame_arrived` already discards surplus frames before any GPU work; DD simply loses the earlier cut |
| Timestamp unit mix-up | Medium | Convert once at the backend boundary; assert the two backends agree on a known-cadence capture |
| Backend divergence over time | Low | The `FrameSink` trait is the only entry; sinks cannot tell which backend they are on |

## Testing

Unit-testable without hardware:

- QPC ticks → 100 ns conversion, including a `LastPresentTime` of 0
- `capture_method` parsing, including unknown values falling back to `Auto`
- `monitor_index` → (adapter, output) resolution against `probe.rs`'s existing
  enumeration, in the same shape as the current agreement test
- `capture_method` is in `REQUIRES_REARM`, and is a rendered settings field

Hand verification, both backends, same machine:

1. Clip on each backend; compare file size, duration, and audio sync
2. Cursor present and correct in both, including a monochrome cursor
3. Launch a game, confirm the rebuild path recovers (log shows the loss and the
   new session)
4. Hybrid GPU: capture with the game on the dGPU and the display on the iGPU
5. Alt-tab, UAC prompt, and resolution change during capture
6. `stats` line comparison — frame latency and drop counts should be in the
   same range on both

## Phase 0: the spike

**Before any of the above.** The evidence that Desktop Duplication clears the
border is OBS's behaviour, not Trix's, and the whole design rests on it. Prove
it from a Trix process first.

A throwaway `trix-cli` subcommand — `trix-cli dd-probe --monitor 0 --seconds 20`
— that resolves the monitor, creates a device on its adapter, calls
`DuplicateOutput`, runs an acquire/release loop counting frames, and prints the
count. No sink, no seam, no encoder, no cursor, no ring. On the order of 150
lines, thrown away afterwards.

Ship it to the reporting user and have them run it while R6 is open. It answers
four things at once:

1. Does Desktop Duplication clear the border **from Trix's process**?
2. Does it work at all on their machine and driver?
3. Does it survive R6 in each display mode, or does it lose access constantly?
4. Roughly what frame cadence does it deliver?

If the border is still there, this design is dead and two weeks are saved. That
is the point of it.

## Effort

A week or two of focused work after the spike. The seam and the WGC re-wrap are
the easy half and could land on their own with no behaviour change at all.
Cursor compositing is now the only item that can eat several days on its own,
and it is the one the automated gates cannot catch.

## Open questions

1. ~~Does Windows 10 draw the border for display capture?~~ **Answered:** not
   every time. Recorded above; Ship 2 is no longer urgent because of it.
2. ~~Does Desktop Duplication clear the border on the reporting user's
   machine?~~ **OBS says yes.** Phase 0 confirms it from Trix's own process
   before the backend is built.
3. Should `Automatic` prefer Desktop Duplication when another app is forcing
   the border? There is no API to query that, so no — the setting stays the
   only way, and the help text carries the diagnosis.
4. Does the cursor need to reach parity with WGC before Ship 1, or can Desktop
   Duplication ship cursor-less behind an opt-in setting and gain it after? A
   user choosing this backend to escape a border may well accept no cursor;
   nobody has been asked.
