# Desktop Duplication capture backend — design

**Status:** **Phase 0 complete. Ship 1 in progress** — the `FrameSink` seam
and the WGC re-wrap are built and hand-verified on `feat/frame-sink-seam`; the
Desktop Duplication backend and the `capture_method` setting are next.
No open questions.
Desktop Duplication clears the border — proven from a Trix process on the
reporting user's own machine — survives repeated alt-tabs (4 of 4 recovered,
0.4 s), survives the secure desktop (3 of 3 recovered, 0.1 s unobstructed), and
recovers **without recreating the D3D11 device**, which is what lets the sink,
encoder, mixer and replay ring live through a transition. See **Phase 0**.
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
- Cursor capture on the Desktop Duplication backend, in Ship 1. Decided and
  deferred — see **Cursor** below.

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

**Built, on `feat/frame-sink-seam`.** `capture/source.rs` is the seam,
`capture/wgc.rs` is the backend, and all three sinks are through it —
`border_settings` and `min_update_interval` are now private to the capture
module, which is the check that nothing else reaches WGC. Verified on this
machine, not just compiled: a full-resolution PNG snapshot, a 6 s recording
(357 frames, 0 dropped, audio in sync), and an 8 s replay clip with its
thumbnail.

Four things the design did not anticipate, all small:

- **`CaptureControl::callback()` hands back a `parking_lot::Mutex`,** which
  cannot be named without a new direct dependency. So `CaptureHandle` owns the
  sink itself, behind a `std::sync::Mutex` whose guard recovers from poisoning
  — a sink that panics mid-frame must still be reachable, or `record.rs` can
  never call `finish()` and the MP4 keeps no `moov` atom.
- **The probe could not come along for free.** `Frame::save_as_image` is a
  windows-capture method, so the snapshot needed its own PNG writer;
  `thumb.rs`'s WIC encoder was parameterised by container to provide one, and
  `ReplaySession::stage_thumbnail` moved out to `capture::stage::stage_bgra` so
  both callers share the copy out of VRAM.
- **The seam owns monitor lookup** (`source::monitor_info`), so `replay.rs` and
  `record.rs` no longer import `windows_capture::monitor::Monitor` for their
  encoder dimensions. Desktop Duplication takes its geometry from
  `DXGI_OUTPUT_DESC` instead, and nothing above the seam has to care.
- **A frame with no readable timestamp is now skipped for all three sinks.**
  The ring and the recorder already did; the probe used to save its snapshot
  anyway. A frame that cannot be placed on the timeline cannot be encoded.

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
fn qpc_to_100ns(ticks: i64, qpf: i64) -> i64 {
    ((ticks as i128 * 10_000_000) / qpf as i128) as i64
}
```

**The 128-bit intermediate is required, not defensive.** `LastPresentTime` is
an *absolute* QPC value counting from boot, so on the 10 MHz timer this machine
reports it passes 8.6e11 after a day of uptime — and multiplying that by
10,000,000 overflows `i64`. The obvious one-line version of this expression
wraps to a negative timestamp on any machine that has not rebooted recently,
which is the hardest possible bug to reproduce on a developer machine that
reboots daily. Phase 0 carries the function and the test that pins it; Ship 1
inherits both verbatim.

Both clocks are QPC-based, so after conversion they are the same timeline the
audio mixer's `t0` and the encoder's `pts` already use. `LastPresentTime` can
be 0 when only the mouse moved — those frames carry no new desktop content and
are skipped rather than timestamped from the wall clock.

### Cursor — deliberately absent in Ship 1

**Decided: Desktop Duplication ships without a cursor.** It is the single
largest work item in this document, it is entirely separable, and it is the
difference between one ship and two.

WGC composites the cursor for us (`CursorCaptureSettings::WithCursor`).
Desktop Duplication does not: it delivers the pointer **position** in
`DXGI_OUTDUPL_FRAME_INFO::PointerPosition` and the pointer **shape**
separately via `GetFramePointerShape`, and expects the application to draw it
itself.

The trade is honest for who this backend is for. Somebody selects it because
Windows is drawing a yellow border across their game; a missing mouse pointer
in a shooter — where the cursor is hidden anyway — is a small price, and the
default backend still has one. **But it is only honest if it is disclosed**,
which is why the option label and help text below both carry it. A user who
picks this and later finds their clips have no cursor, with nothing having
warned them, files a bug and is right to.

This applies to screenshots too: Trix takes them from the capture that is
already running, so a screenshot on this backend has no cursor either.

**Deferred work, for the follow-up ship:**

- Cache the pointer shape; it is only re-sent when it changes
  (`PointerShapeBufferSize > 0`), so a backend that only reads it when present
  loses the cursor on almost every frame.
- Handle all three shape types: `MONOCHROME` (1-bpp AND/XOR mask, needs real
  masking), `COLOR` (BGRA, straightforward), `MASKED_COLOR` (per-pixel choice
  between copy and XOR). Monochrome is rare in games but is what Windows falls
  back to.
- Skip compositing when `PointerPosition.Visible` is false — another output
  owns the pointer.
- Composite into a copy of the frame, never into the duplication texture —
  writing into the surface DXGI handed us is not ours to do.

When it lands, the option label and help text lose their cursor warning in the
same commit. A stale warning is its own defect.

### Access loss

`AcquireNextFrame` returns `DXGI_ERROR_ACCESS_LOST` on desktop switches, UAC
prompts, mode changes, and fullscreen transitions. This is routine, not
exceptional — a game launching is one of the causes.

**The backend recovers internally. It does not surface access loss as session
death.** An earlier draft of this section said the opposite — reuse
`run_driven_inner`'s rebuild loop, one recovery path for everything — and that
is wrong for this backend, badly enough to make it unusable.

`run_driven_inner` ([replay.rs:899](../../../crates/trix-core/src/replay.rs))
is built for display topology changes, which are rare. It adds a **2 s settle**,
**restarts the replay ring empty**, drops queued clip requests, and charges a
failure-budget slot against any session that dies within 10 seconds — thirty of
those and capture stops permanently. On WGC that is right. On Desktop
Duplication, **every alt-tab is two access losses**, so that policy would empty
a user's replay buffer twice each time they tabbed out to Discord and back, and
a few rapid alt-tabs could exhaust the budget outright. Phase 0 measured the
actual outage at **0.4 s**.

So the acquire loop absorbs the loss itself, and only genuine death — device
removed, monitor gone, repeated failure to reopen — escalates to session death
and the existing loop.

**Recovery must keep the D3D11 device wherever it can.** This is the constraint
that makes internal recovery possible at all: the sink's `VideoConverter` is
built against the capture device and blits every frame into its own NV12 pool,
and a texture from one device cannot be used on another. A recovery that
recreates the device therefore invalidates the sink, and with it the encoder,
the audio mixer and the ring — which is session death by another name. Keeping
the device makes the whole thing invisible above the seam.

So the recovery is tiered:

1. Release the duplication, re-enumerate from a fresh factory, and re-duplicate
   **onto the existing device**. Ride out a burst this way — a transition
   produces several losses in a row.
2. Only if that keeps failing, create a new device. Correct when the adapter
   itself has changed, and honest about its cost: this tier takes the sink down.

**A refused reopen is temporary.** `DuplicateOutput` returns `E_ACCESSDENIED`
while the secure desktop is up — a UAC prompt, Ctrl+Alt+Del, the lock screen —
because no user process may duplicate it. That is correct and expected, and it
must be a backed-off retry, never a failure that ends the capture. Phase 0's
probe treated it as fatal and exited on the first Ctrl+Alt+Del; a backend that
did the same would die on every UAC prompt, which users see far more often.
`DXGI_ERROR_DEVICE_REMOVED` is the same kind of event with a different answer:
recoverable, but only on a new device, so it goes straight to tier 2.

**Release before rebuilding, always.** Both failing Phase 0 versions created the
replacement duplication while the dead one was still alive, leaving two
duplications of one output open in the process — and DXGI returns the second
looking valid while every `AcquireNextFrame` on it fails, permanently. 37 losses
in one run and 18 in the next, zero recoveries in either. With the ordering
fixed, the same test recovered 4 of 4.

**A stale factory is not the cause, and the diagnosis that said so was wrong.**
`IDXGIFactory1::IsCurrent` returned **true** at every one of those 18 losses and
`GetDeviceRemovedReason` reported the device healthy. Re-enumerating from a
fresh factory is cheap and is what the spike does, but it fixes nothing on its
own; the ordering is what matters.

The stale state itself is real, though — `IsCurrent` came back **false** on a
Ctrl+Alt+Del. So re-enumerating on every reopen stays, on its own merits rather
than as a fix for something it never fixed.

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
    { value: 'dd', label: 'Desktop Duplication - no mouse cursor' },
  ], help: '...' },
```

The help text has to earn its place, because this is the one setting where a
user must self-diagnose, and the one where choosing it costs them something.
It should name the symptom rather than the API, and state the cost plainly —
to the effect of: *"If Windows draws a yellow border around your screen while
Trix is armed, choose Desktop Duplication. It cannot record the mouse cursor."*

The cursor warning belongs in **both** places. The label is what a user reads
while choosing; the help is what they read afterwards when wondering why the
pointer is gone.

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
| No cursor on this backend | Medium, accepted | Out of scope for Ship 1 by decision. Disclosed in the option label *and* the help text, so it is a stated trade rather than a surprise |
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
2. Cursor present on WGC, absent on Desktop Duplication, and the settings row
   said so before the user chose it
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

**Built**, on `spike/dd-probe`, as `trix dd-probe [--monitor N] [--seconds N]`.
With no `--monitor` it targets the screen the config already captures, and it
takes no single-instance lock, because the case worth measuring is running it
while Trix is armed.

Answered on the developer machine (hybrid GPU, Intel UHD driving the panel):

| Question | Answer here |
| --- | --- |
| Does duplication work at all? | Yes — 60–105 desktop frames/s, no access losses, no wait timeouts |
| Adapter routing on a hybrid GPU | **Resolved correctly.** Monitor 0 → adapter 0, the iGPU driving the output, and `DuplicateOutput` succeeded. The design's largest technical risk, clear on the configuration it was worried about |
| What format does it deliver? | `B8G8R8A8_UNORM` — the same format `VideoConverter` already takes from WGC, so nothing downstream changes |
| How many frames are mouse-only? | Over half, in a run with an active pointer. Confirms skipping `LastPresentTime == 0` rather than timestamping those |

### Answered: no border

**2026-09-06, on the reporting user's machine: Desktop Duplication produced no
yellow border.** The run behind that sentence is worth stating precisely,
because the first attempt at it was void:

| | |
| --- | --- |
| Run length | 30.0 s, of which **30.0 s actually capturing** |
| Access losses | 0 |
| Frames | 1802 desktop (60.0/s), 884 mouse-only |
| Present gap | min 14.4 ms, avg 16.7 ms, max 19.0 ms — a hard 60 Hz lock |
| Surface | 1920x1080 `B8G8R8A8_UNORM`, on the RTX 3060 driving the display |
| Border | **none** |

The cadence is better than expected: ±2 ms of jitter against a 16.7 ms period,
with no `MinUpdateInterval` equivalent needed to get there.

**The first attempt was void and is not evidence.** It began capturing the
instant it launched, the tester spent the first ten seconds alt-tabbing, and
the duplication was dead by the time they were looking at the game — so their
"no border" described a period when nothing was capturing at all. The probe now
counts the tester in before opening anything, measures how long it was actually
live, and refuses to call a run below two thirds an answer. **A remote test that
cannot detect that it measured nothing will hand you a confident wrong answer.**

### Not yet tested: recovery from a transition

The clean run is also a blind spot. It had zero access losses because the
tester was already in the game before duplication opened, so no fullscreen
transition ever happened — which means **the rebuild path above never
executed.** Armed Trix runs continuously while people alt-tab in and out of
games, so that transition is not an edge case, it is the normal case.

Two probe runs covered it. The first **failed** — three alt-tabs, 18 access
losses, **zero recoveries**, dark for 23 of 31 seconds — and the cause was the
rebuild ordering described in **Access loss** above. With that fixed:

| | |
| --- | --- |
| Run length | 40.0 s, of which **38.4 s capturing** (96%) |
| Alt-tabs | three, out and back |
| Access losses | 4 — one per transition, in each direction |
| Recovered | **4 of 4** |
| Worst blackout | **0.4 s** |
| Longest unbroken | 19.9 s |

The log identifies the losses precisely: the foreground window is empty at the
moment of each outbound transition and back to `"Rainbow Six" 1920x1080` on the
return. These are the alt-tabs and nothing else.

**0.4 s is the number the design turns on.** It is small enough to absorb inside
the backend and far too small to justify taking the session down for — see
**Access loss** for what that policy would have cost.

### Confirmed: recovery keeps the device, so the sink survives

Tested on the developer machine with Ctrl+Alt+Del, which is a harder event than
an alt-tab — the secure desktop refuses duplication to everyone while it is up.

| | |
| --- | --- |
| Access losses | 3 |
| Recovered | **3 of 3** |
| Kept the D3D11 device | **3 of 3** — none needed a new one |
| Reopens refused and retried | 12, all `E_ACCESSDENIED` |
| Recovery latency, unobstructed | **0.1 s** |
| Worst blackout | 2.7 s |

**Read the 2.7 s correctly.** That is how long the secure desktop was up, not
how long recovery took. While Windows holds that desktop nothing may capture it
— WGC included — so it is not a cost this backend imposes, and the shortest
recovery, with nothing blocking, was 0.1 s. The probe's own verdict line does
not draw that distinction; a reader of a future log should.

**Every Phase 0 question is now closed**, including the one Ship 1's
architecture depends on: recovery is invisible above the seam, so the sink, the
encoder, the mixer and the replay ring all survive a transition. Internal
recovery is viable as specified.

## Effort

**Ship 1, cursor-less: around a week** after the spike. The seam and the WGC
re-wrap are the easy half and could land on their own with no behaviour change
at all; the duplication backend without cursor compositing is an acquire loop,
a timestamp conversion, and a config key.

Deferring the cursor takes roughly a third off, and takes the whole of the
`GetFramePointerShape` masking work — the part with no automated gate — out of
the first release entirely.

**Cursor follow-up: several days**, whenever it is wanted, with no dependency
on anything else shipping first.

## Open questions

1. ~~Does Windows 10 draw the border for display capture?~~ **Answered:** not
   every time. Recorded above; Ship 2 is no longer urgent because of it.
2. ~~Does Desktop Duplication clear the border on the reporting user's
   machine?~~ **Answered 2026-09-06: no border**, measured from Trix's own
   process over 30 s of verified-live capture. See **Phase 0**. This was the
   question the whole design rested on.
   *Still open beneath it:* does the backend recover from a fullscreen
   transition? The run that answered the border question never had one.
3. Should `Automatic` prefer Desktop Duplication when another app is forcing
   the border? There is no API to query that, so no — the setting stays the
   only way, and the help text carries the diagnosis.
4. ~~Does the cursor need parity with WGC before Ship 1?~~ **Answered: no.**
   Desktop Duplication ships cursor-less behind the opt-in setting and gains
   the cursor in a follow-up. Disclosed in the option label and the help text.
