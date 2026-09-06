//! DXGI Desktop Duplication, behind the [`FrameSink`] seam.
//!
//! The reason this backend exists is a border. Windows paints a yellow
//! "you are being captured" frame around the screen while a Graphics Capture
//! session is running and some other process has asked for it — the property
//! belongs to the *display*, so a `true` from any one app beats Trix's
//! `false`, and there is no fix inside WGC. Desktop Duplication is not subject
//! to it. See
//! `docs/superpowers/specs/2026-09-06-trix-desktop-duplication-design.md`.
//!
//! **The texture is DXGI's until `ReleaseFrame`.** That is the invariant the
//! borrow in [`SourceFrame`] encodes, and it is the difference between this
//! backend and WGC: a sink must finish with the frame before `on_frame`
//! returns. Every sink already does — the NV12 converter blits into its own
//! pool, the thumbnail path copies to system memory — but nothing new may
//! start.
//!
//! # What Phase 0 paid for
//!
//! Three findings, each of which cost a remote diagnostic round-trip through a
//! user in another country, and each of which is load-bearing here:
//!
//! 1. **Release the dead duplication before creating its replacement.** Two
//!    live duplications of one output leave DXGI handing back a second that
//!    looks valid and fails every acquire, permanently. Two spike versions did
//!    it the other way: 37 losses and 18 losses, zero recoveries between them.
//!    With the ordering fixed the same test recovered 4 of 4, in 0.4 s each.
//! 2. **A refused reopen is temporary.** `DuplicateOutput` returns
//!    `E_ACCESSDENIED` for as long as the secure desktop is up — a UAC prompt,
//!    Ctrl+Alt+Del, the lock screen — and no user process may duplicate it.
//!    Retried, never fatal: a backend that gave up here would stop recording
//!    every time Windows asked for consent to anything.
//! 3. **Recovery keeps the D3D11 device, or it is not recovery.** The sink's
//!    `VideoConverter` is built against the capture device and a texture
//!    cannot cross devices, so a rebuild that made a new device would
//!    invalidate the sink, the encoder, the mixer and the replay ring — which
//!    is session death wearing a different hat. Phase 0 recovered 3 of 3
//!    across the secure desktop *keeping* the device, which is what makes the
//!    recovery in this file invisible from above.
//!
//! Access loss is routine here in a way it never was on WGC: **every alt-tab
//! is two of them**. Surfacing them upward would empty the user's replay ring
//! twice per alt-tab for an outage of under half a second, so this loop
//! absorbs them and only genuine death — the device removed, the output gone,
//! reopening failing for a reason that is not the secure desktop — ends the
//! session and lets the engine rebuild.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::core::Interface as _;

use super::source::{Flow, FrameSink, SinkRef, SourceFrame};

/// How long `AcquireNextFrame` waits before reporting no new frame.
///
/// Also the loop's responsiveness to a stop request, since that is checked once
/// per pass. Short enough that shutdown never visibly hangs; long enough that a
/// static screen costs ten wake-ups a second rather than thousands.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// Backoff between reopen attempts, doubling from the first to the second.
///
/// The ceiling is deliberately low. A secure desktop can stay up for minutes
/// and we simply wait it out, but the *recovery* it is hiding takes about
/// 0.4 s — so a coarse backoff would spend most of a transition asleep rather
/// than recording.
const BACKOFF_START_MS: u64 = 100;
const MAX_BACKOFF_MS: u64 = 500;

/// The delivery cut this backend does for itself, as a fraction of a frame
/// period, mirroring `super::min_update_interval`'s reasoning exactly: a full
/// period beats against the compositor's own cadence and halves the delivery
/// rate, so the gate sits at three quarters of one and the sink's QPC pacer
/// does the fine work.
///
/// Measured, and it earns its place for a different reason than expected.
/// Without it Desktop Duplication hands the sink **133 frames a second** on a
/// 60 fps target, where WGC's `MinUpdateInterval` delivers 75. A/B runs put
/// the *throughput* either side of the noise — 45.8 and 42.5 fps gated against
/// 44.1 and 46.7 ungated — so this does not make the encoder faster. What it
/// removes is waste: 68 and 81 dropped frames gated, against 634 and 381
/// ungated. Every one of those was an acquire, a copy and a converter call
/// spent on a frame nothing could accept.
///
/// The sink's own pacer cannot do this job, which is why it belongs here: it
/// re-anchors its schedule to the last frame it actually *encoded*, so once
/// the encoder starts refusing, the pacer stops pacing altogether.
const DELIVERY_GATE_NUMERATOR_100NS: i64 = 7_500_000;

/// Consecutive reopen failures that are **not** the secure desktop, before the
/// session is declared dead and handed back to the engine to rebuild.
///
/// A refusal from the secure desktop is not counted at all: it is a state the
/// user is in, not an error, and it ends when they dismiss the prompt. What
/// this bounds is the other kind — an adapter that changed under us, an output
/// that went away — where retrying forever would silently record nothing.
const MAX_REOPEN_FAILURES: u32 = 30;

/// Converts a QPC tick count into the 100 ns units the rest of the pipeline
/// already uses.
///
/// WGC's `SystemRelativeTime` is *already* 100 ns; Desktop Duplication's
/// `LastPresentTime` is raw QPC. The two are not interchangeable, and mixing
/// them desynchronises audio against video.
///
/// The 128-bit intermediate is not defensive padding. `LastPresentTime` is an
/// **absolute** QPC value counting from boot, so on a 10 MHz timer it passes
/// 8.6e11 after a day of uptime — and multiplying that by 10,000,000 overflows
/// `i64`. A machine awake since Tuesday would silently produce negative
/// timestamps, which is the hardest possible bug to reproduce on a developer
/// machine that reboots daily.
fn qpc_to_100ns(ticks: i64, qpf: i64) -> i64 {
    debug_assert!(qpf > 0, "QueryPerformanceFrequency never reports zero");
    ((ticks as i128 * 10_000_000) / qpf as i128) as i64
}

/// The adapter and output that `monitor_index` names.
struct Target {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    name: String,
    /// DXGI's desktop rectangle, which is **DPI-virtualised** — a 1920x1200
    /// screen at 125% enumerates here as 1536x960. Logged, never used to size
    /// anything: the encoder is sized from the display mode (see
    /// `probe::monitors_true_pixels`) and each frame carries its real size in
    /// the texture description.
    width: u32,
    height: u32,
}

/// Walks DXGI the way [`crate::probe`] does, and stops at `monitor_index`.
///
/// The counting rule is the contract: desktop-attached outputs only, counted
/// **across** adapters in DXGI's order, because that is what
/// `config.monitor_index` stores and what the settings dropdown binds to.
///
/// Returning the *adapter* is the entire point. Creating the device on the
/// default adapter instead works on a desktop and fails with
/// `DXGI_ERROR_UNSUPPORTED` on a hybrid-GPU laptop whose display hangs off the
/// iGPU — and that is the machine this was written on.
///
/// Takes the factory rather than making one, so a reopen can hand it a fresh
/// one: `IDXGIFactory1::IsCurrent` was observed going false across a
/// Ctrl+Alt+Del, and an enumeration from a stale factory describes a desktop
/// that has moved on.
fn resolve_output(factory: &IDXGIFactory1, monitor_index: u32) -> Result<Target> {
    let mut seen = 0u32;
    let mut adapter_index = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let mut output_index = 0u32;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            let desc = unsafe { output.GetDesc() }.context("IDXGIOutput::GetDesc")?;
            if desc.AttachedToDesktop.as_bool() {
                if seen == monitor_index {
                    let r = desc.DesktopCoordinates;
                    return Ok(Target {
                        adapter,
                        output,
                        name: wide_to_string(&desc.DeviceName),
                        width: (r.right - r.left).unsigned_abs(),
                        height: (r.bottom - r.top).unsigned_abs(),
                    });
                }
                seen += 1;
            }
            output_index += 1;
        }
        adapter_index += 1;
    }

    bail!(
        "monitor {monitor_index} does not exist — DXGI reports {seen} desktop-attached \
         monitor(s). Run `trix probe` to list them."
    )
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Creates a D3D11 device **on the adapter driving this output**.
///
/// `D3D_DRIVER_TYPE_UNKNOWN` is required rather than preferred: passing an
/// adapter together with any other driver type is an invalid-argument error.
fn create_device(adapter: &IDXGIAdapter1) -> Result<ID3D11Device> {
    let mut device: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .context("D3D11CreateDevice on the adapter driving this monitor failed")?;
    device.context("D3D11CreateDevice reported success but handed back no device")
}

/// Opens the duplication, translating the failures a user can actually hit.
///
/// These are worth naming because they are the difference between "this
/// machine cannot do it", "close OBS", "you are on remote desktop", and "there
/// is a UAC prompt up right now". Anything else is a genuine surprise and is
/// reported raw.
fn duplicate(
    output: &IDXGIOutput,
    device: &ID3D11Device,
) -> std::result::Result<IDXGIOutputDuplication, OpenError> {
    let output1: IDXGIOutput1 = output.cast().map_err(|e| {
        OpenError::Fatal(anyhow!(
            "this output does not support IDXGIOutput1, so it cannot be duplicated: {e}"
        ))
    })?;

    unsafe { output1.DuplicateOutput(device) }.map_err(|e| {
        let code = e.code();
        if code == E_ACCESSDENIED {
            // The secure desktop: a UAC prompt, Ctrl+Alt+Del, or the lock
            // screen. No user process may duplicate it, and it ends when the
            // user answers. Classified here, at the only place that can read
            // the HRESULT, so the loop never has to guess from a string.
            return OpenError::SecureDesktop;
        }
        OpenError::Fatal(if code == DXGI_ERROR_UNSUPPORTED {
            anyhow!(
                "DXGI_ERROR_UNSUPPORTED — this display cannot be duplicated from the adapter that \
                 drives it. A hybrid-GPU laptop is the usual cause; use Windows Graphics Capture \
                 on this machine."
            )
        } else if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
            anyhow!(
                "DXGI_ERROR_NOT_CURRENTLY_AVAILABLE — this display already has the maximum number \
                 of duplications open. Close OBS or any other duplication-based recorder."
            )
        } else if code == DXGI_ERROR_SESSION_DISCONNECTED {
            anyhow!(
                "DXGI_ERROR_SESSION_DISCONNECTED — there is no interactive desktop to duplicate \
                 (a remote-desktop connection, a locked screen, or a service context)"
            )
        } else {
            anyhow!("DuplicateOutput failed: {e}")
        })
    })
}

/// Why a duplication could not be opened.
///
/// The distinction is the whole of Phase 0's second finding. A refusal from the
/// secure desktop is a *state the user is in*, not a failure, and it ends on
/// its own; anything else is a real problem that must eventually stop the
/// session rather than be retried forever into a recording nobody is getting.
enum OpenError {
    SecureDesktop,
    Fatal(anyhow::Error),
}

impl From<OpenError> for anyhow::Error {
    fn from(e: OpenError) -> Self {
        match e {
            OpenError::SecureDesktop => anyhow!(
                "the Windows secure desktop is up (a UAC prompt, Ctrl+Alt+Del, or the lock                  screen), and no application may duplicate it"
            ),
            OpenError::Fatal(e) => e,
        }
    }
}

/// The live duplication, the device it was built on, and the texture frames
/// are copied into.
struct Chain {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dupl: IDXGIOutputDuplication,
    /// Our own copy of the desktop image, and the only thing the sink ever
    /// sees.
    ///
    /// DXGI's surface belongs to DXGI until `ReleaseFrame`, and handing it
    /// straight to the encoder puts a frame's worth of GPU work between the
    /// acquire and the release on every single frame. Measured on this
    /// machine, that halved throughput: the encoder took 27 frames a second
    /// against WGC's 58 on the same screen, dropping 151 of 320. Copying into
    /// a texture we own lets the release happen immediately and the encode
    /// happen on our own schedule.
    ///
    /// Rebuilt only when the desktop resolution changes, so the steady state
    /// is one GPU-to-GPU blit per frame and no allocation.
    copy: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Chain {
    /// Opens a duplication, on `reuse` if one is offered.
    ///
    /// Reusing the device is the whole reason recovery can be invisible: the
    /// sink was built against it, and a texture from one D3D11 device cannot be
    /// used on another. So there is no second tier here — if the device itself
    /// has to change, the session ends and the engine rebuilds everything on
    /// top of a new one.
    ///
    /// The factory is created fresh every time rather than kept.
    /// `IDXGIFactory1::IsCurrent` was seen returning false across a
    /// Ctrl+Alt+Del, and re-enumerating costs microseconds on a path that only
    /// runs when something has already gone wrong.
    fn open(
        monitor_index: u32,
        reuse: Option<ID3D11Device>,
    ) -> std::result::Result<Self, OpenError> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|e| OpenError::Fatal(anyhow!("CreateDXGIFactory1 failed: {e}")))?;
        let target = resolve_output(&factory, monitor_index).map_err(OpenError::Fatal)?;
        let device = match reuse {
            Some(device) => device,
            None => create_device(&target.adapter).map_err(OpenError::Fatal)?,
        };
        let context = unsafe { device.GetImmediateContext() }
            .map_err(|e| OpenError::Fatal(anyhow!("no immediate context: {e}")))?;
        let dupl = duplicate(&target.output, &device)?;
        tracing::debug!(
            monitor = %target.name,
            dxgi_width = target.width,
            dxgi_height = target.height,
            "desktop duplication open"
        );
        Ok(Self { device, context, dupl, copy: None })
    }

    /// Copies the acquired desktop image into a texture we own, so DXGI's can
    /// be released before any encoding starts.
    fn copy_out(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };

        let matches =
            matches!(&self.copy, Some((_, w, h)) if *w == desc.Width && *h == desc.Height);
        if !matches {
            let own_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            let mut texture: Option<ID3D11Texture2D> = None;
            unsafe { self.device.CreateTexture2D(&own_desc, None, Some(&mut texture)) }
                .context("CreateTexture2D for the duplication copy")?;
            let texture = texture.context("CreateTexture2D handed back no texture")?;
            tracing::debug!(width = desc.Width, height = desc.Height, "duplication copy target");
            self.copy = Some((texture, desc.Width, desc.Height));
        }

        let (texture, _, _) = self.copy.as_ref().expect("the copy target was just ensured");
        unsafe { self.context.CopyResource(texture, source) };
        Ok(texture.clone())
    }
}

/// A running Desktop Duplication session. Owns the acquire thread.
pub(super) struct Session<S: FrameSink> {
    thread: Option<JoinHandle<Result<()>>>,
    halt: Arc<AtomicBool>,
    _sink: std::marker::PhantomData<S>,
}

impl<S: FrameSink> Session<S> {
    pub(super) fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Asks the acquire loop to end and waits for it.
    pub(super) fn stop(mut self) -> Result<()> {
        self.halt.store(true, Ordering::Relaxed);
        self.join()
    }

    /// Waits for a loop that is already ending, and reports why it ended.
    pub(super) fn wait(mut self) -> Result<()> {
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| anyhow!("the capture thread panicked"))?,
            None => Ok(()),
        }
    }
}

pub(super) fn start<S: FrameSink>(
    monitor_index: u32,
    pace_to_fps: Option<u32>,
    flags: S::Flags,
) -> Result<(Session<S>, SinkRef<S>)> {
    let halt = Arc::new(AtomicBool::new(false));
    let loop_halt = Arc::clone(&halt);
    let (ready, published) = mpsc::channel();

    let thread = std::thread::Builder::new()
        .name("trix-duplication".into())
        .spawn(move || acquire_loop::<S>(monitor_index, pace_to_fps, flags, &loop_halt, &ready))
        .context("could not start the duplication capture thread")?;

    // The loop publishes before its first acquire, so this does not wait on a
    // frame — only on the device, the duplication and the sink being built,
    // which is exactly what the WGC backend reports synchronously too.
    match published.recv() {
        Ok(Ok(sink)) => Ok((Session { thread: Some(thread), halt, _sink: PhantomData }, sink)),
        // The loop already returned this error; joining is what stops the
        // thread being left behind, and its result is redundant here.
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let joined = thread.join();
            Err(anyhow!("the duplication capture thread ended before it started: {joined:?}"))
        }
    }
}

/// Opens the chain, builds the sink on its device, then acquires until asked to
/// stop or until something happens that this loop cannot absorb.
fn acquire_loop<S: FrameSink>(
    monitor_index: u32,
    pace_to_fps: Option<u32>,
    flags: S::Flags,
    halt: &AtomicBool,
    ready: &Sender<Result<SinkRef<S>>>,
) -> Result<()> {
    // Join the multithreaded apartment before touching anything.
    //
    // Everything this thread goes on to drive is COM: DXGI, D3D11, and — once
    // the sink is built on our device — the Media Foundation encoder whose
    // event queue we drain from inside `on_frame`. windows-capture's own
    // capture thread does the equivalent (`RoInitialize(RO_INIT_MULTITHREADED)`)
    // before its message loop, and leaving it out here was measurable rather
    // than theoretical: the encoder handed back **no input credits**, so the
    // replay ring dropped 53 of 89 frames and the recorder 30 of 58 while the
    // pool sat nearly empty. `S_FALSE` means this thread is already in the
    // MTA, which is success.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("could not join the multithreaded apartment")?;

    let mut qpf = 0i64;
    unsafe { QueryPerformanceFrequency(&mut qpf) }.context("QueryPerformanceFrequency")?;

    let chain = match Chain::open(monitor_index, None) {
        Ok(chain) => chain,
        Err(e) => {
            let _ = ready.send(Err(anyhow::Error::from(e)));
            return Ok(());
        }
    };
    let sink = match S::new(&chain.device, flags) {
        Ok(sink) => SinkRef::new(sink),
        Err(e) => {
            let _ = ready.send(Err(e));
            return Ok(());
        }
    };
    if ready.send(Ok(sink.clone())).is_err() {
        // The caller gave up between spawning and here. Nothing to capture for.
        return Ok(());
    }
    tracing::info!(monitor_index, "desktop duplication capturing (no cursor on this backend)");

    let outcome =
        run_frames(monitor_index, delivery_gate_100ns(pace_to_fps), halt, &sink, chain, qpf);
    if let Err(e) = sink.lock().on_closed() {
        tracing::warn!(error = %format!("{e:#}"), "capture sink did not close cleanly");
    }
    outcome
}

fn run_frames<S: FrameSink>(
    monitor_index: u32,
    delivery_gate_100ns: i64,
    halt: &AtomicBool,
    sink: &SinkRef<S>,
    chain: Chain,
    qpf: i64,
) -> Result<()> {
    // The device outlives every chain built on it. Losing it is what the sink
    // cannot survive, so it is held here and handed to each reopen.
    let device = chain.device.clone();
    let mut chain = Some(chain);
    let mut backoff_ms = BACKOFF_START_MS;
    let mut reopen_failures = 0u32;
    let mut losses = 0u64;
    let mut dark_since: Option<Instant> = None;
    let mut last_delivered_100ns: Option<i64> = None;

    while !halt.load(Ordering::Relaxed) {
        let active = match &mut chain {
            Some(active) => active,
            None => {
                match Chain::open(monitor_index, Some(device.clone())) {
                    Ok(reopened) => {
                        let dark = dark_since.take().map(|at| at.elapsed()).unwrap_or_default();
                        tracing::info!(
                            losses,
                            dark_ms = dark.as_millis() as u64,
                            "duplication recovered on the same device"
                        );
                        reopen_failures = 0;
                        backoff_ms = BACKOFF_START_MS;
                        chain = Some(reopened);
                    }
                    // Waiting this out is the correct behaviour, and the
                    // failure budget deliberately does not apply: a user can
                    // leave a UAC prompt on screen for as long as they like.
                    Err(OpenError::SecureDesktop) => {
                        tracing::debug!("duplication refused while the secure desktop is up");
                    }
                    Err(OpenError::Fatal(e)) => {
                        reopen_failures += 1;
                        if reopen_failures >= MAX_REOPEN_FAILURES {
                            return Err(e).context(
                                "desktop duplication could not be reopened; rebuilding the capture \
                                 session",
                            );
                        }
                        tracing::warn!(
                            attempt = reopen_failures,
                            error = %format!("{e:#}"),
                            "could not reopen desktop duplication"
                        );
                    }
                }
                if chain.is_none() {
                    std::thread::sleep(Duration::from_millis(backoff_ms));
                    backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
                }
                continue;
            }
        };

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let acquired =
            unsafe { active.dupl.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource) };

        match acquired {
            Ok(()) => {
                // Copy out, release, *then* encode. DXGI's surface is handed
                // back before any of our own GPU work is queued against it,
                // which is what keeps the acquire loop and the encoder from
                // serialising against each other every frame.
                let frame = claim(
                    active,
                    resource.as_ref(),
                    &info,
                    qpf,
                    delivery_gate_100ns,
                    &mut last_delivered_100ns,
                );
                let released = unsafe { active.dupl.ReleaseFrame() };
                let frame = frame?;
                released.context("ReleaseFrame failed")?;
                if let Some((texture, present_100ns)) = frame {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    unsafe { texture.GetDesc(&mut desc) };
                    let flow = sink.lock().on_frame(SourceFrame {
                        texture: &texture,
                        qpc_100ns: present_100ns,
                        width: desc.Width,
                        height: desc.Height,
                    })?;
                    if flow == Flow::Stop {
                        return Ok(());
                    }
                }
            }
            // No new frame in the timeout. Normal: like WGC, duplication only
            // delivers on change, and a still screen delivers nothing.
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                losses += 1;
                // `get_or_insert`, not a fresh stamp: a fullscreen transition
                // is a burst of losses, and that is one dark stretch as far as
                // the recording is concerned, not six.
                dark_since.get_or_insert_with(Instant::now);
                tracing::debug!(losses, "duplication access lost — reopening");
                // Released here, before the replacement is built. Two live
                // duplications of one output make DXGI hand back a second that
                // looks valid and fails every acquire, permanently — the exact
                // shape of the two Phase 0 runs that never recovered.
                chain = None;
                std::thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
            // The driver reset. Recoverable, but only on a new device — which
            // is the one thing this loop cannot do without invalidating the
            // sink built on the old one. Ending the session is how a new
            // device gets made.
            Err(e) if e.code() == DXGI_ERROR_DEVICE_REMOVED => {
                return Err(anyhow!("the graphics device was removed ({e})"))
                    .context("desktop duplication needs a new device; rebuilding the session");
            }
            Err(e) => return Err(e).context("AcquireNextFrame failed"),
        }
    }
    Ok(())
}

/// Takes ownership of one acquired frame, if it carries new desktop content
/// that this backend has not just delivered.
///
/// Split out so the caller can call `ReleaseFrame` on every path, success or
/// not, without a `Drop` guard whose ordering would be harder to see than
/// this. Everything that touches DXGI's surface happens here; the sink is fed
/// afterwards, from our own texture.
fn claim(
    chain: &mut Chain,
    resource: Option<&IDXGIResource>,
    info: &DXGI_OUTDUPL_FRAME_INFO,
    qpf: i64,
    gate_100ns: i64,
    last_delivered_100ns: &mut Option<i64>,
) -> Result<Option<(ID3D11Texture2D, i64)>> {
    // A present time of zero means only the pointer moved. There is no new
    // desktop content behind it, and this backend does not draw the cursor, so
    // there is nothing to encode — and no honest timestamp to encode it at.
    if info.LastPresentTime == 0 {
        return Ok(None);
    }
    let present_100ns = qpc_to_100ns(info.LastPresentTime, qpf);
    // The cut WGC gets from `MinUpdateInterval` and this backend has to make
    // for itself. Skipped before the copy, so a surplus frame costs nothing
    // but the acquire — and because a frame the sink refuses does not advance
    // its own pacing schedule, which is how an unpaced backend swamps the
    // encoder instead of merely outrunning it.
    if let Some(last) = *last_delivered_100ns
        && present_100ns - last < gate_100ns
    {
        return Ok(None);
    }
    let Some(resource) = resource else { return Ok(None) };
    let source: ID3D11Texture2D =
        resource.cast().context("the acquired frame is not an ID3D11Texture2D")?;

    let texture = chain.copy_out(&source)?;
    *last_delivered_100ns = Some(present_100ns);
    Ok(Some((texture, present_100ns)))
}

/// The minimum gap between delivered frames, in 100 ns units.
///
/// `None` — the cadence probe — asks for everything the desktop produces,
/// which is the whole point of measuring.
fn delivery_gate_100ns(pace_to_fps: Option<u32>) -> i64 {
    match pace_to_fps {
        Some(fps) => DELIVERY_GATE_NUMERATOR_100NS / i64::from(fps.max(1)),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion Ship 1 inherited from the Phase 0 spike, with the test
    /// that pins it. A 10 MHz QPC tick is exactly one 100 ns unit, which is
    /// what makes the identity case worth asserting rather than assuming.
    #[test]
    fn qpc_ticks_convert_to_hundred_nanosecond_units() {
        assert_eq!(qpc_to_100ns(10_000_000, 10_000_000), 10_000_000, "one second on a 10 MHz QPC");
        assert_eq!(qpc_to_100ns(1, 10_000_000), 1, "a 10 MHz tick is one 100 ns unit");
        assert_eq!(qpc_to_100ns(3_579_545, 3_579_545), 10_000_000, "one second on a 3.58 MHz QPC");
        assert_eq!(qpc_to_100ns(0, 10_000_000), 0);
    }

    /// `LastPresentTime` counts from boot, so the multiply overflows `i64`
    /// after about a day of uptime. This is the whole reason the intermediate
    /// is `i128`, and a developer machine that reboots nightly would never
    /// catch it.
    #[test]
    fn a_week_of_uptime_does_not_overflow() {
        let qpf = 10_000_000i64;
        let a_week = 7 * 24 * 60 * 60 * qpf;
        assert_eq!(qpc_to_100ns(a_week, qpf), a_week, "a week of ticks must stay positive");
        assert!(qpc_to_100ns(a_week, qpf) > 0);
    }

    /// The gate has to sit *below* a frame period or it beats against the
    /// source's own cadence and halves the delivery rate -- the same reasoning,
    /// and the same three-quarters, as `super::min_update_interval`. It also
    /// has to be off entirely for the cadence probe, which exists to measure
    /// what the backend really produces.
    #[test]
    fn the_delivery_gate_sits_just_under_a_frame_period() {
        let period = |fps: i64| 10_000_000 / fps;
        for fps in [30u32, 60, 120, 144] {
            let gate = delivery_gate_100ns(Some(fps));
            assert!(gate > 0, "{fps} fps must be gated");
            assert!(gate < period(i64::from(fps)), "{fps} fps: a full period would beat");
            assert!(gate > period(i64::from(fps)) / 2, "{fps} fps: half a period lets too much by");
        }
        assert_eq!(delivery_gate_100ns(None), 0, "the cadence probe asks for everything");
        assert_eq!(
            delivery_gate_100ns(Some(0)),
            delivery_gate_100ns(Some(1)),
            "0 fps must not divide by zero"
        );
    }

    /// The index space is `config.monitor_index`: a position among
    /// desktop-attached outputs, counted across adapters. If this backend
    /// counted differently from `probe::monitors`, choosing "Monitor 2" in
    /// Settings would capture a different screen depending on the backend —
    /// a bug no user could diagnose.
    #[test]
    fn the_backend_and_the_settings_list_agree_on_which_monitor_is_which() {
        let listed = crate::probe::monitors().expect("DXGI monitor enumeration must succeed");
        assert!(!listed.is_empty(), "a machine running this test has a desktop attached");

        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.expect("CreateDXGIFactory1 must succeed");
        for monitor in &listed {
            let target =
                resolve_output(&factory, monitor.index).expect("every listed index must resolve");
            assert_eq!(target.name, monitor.name, "monitor {} names disagree", monitor.index);
        }

        assert!(
            resolve_output(&factory, listed.len() as u32).is_err(),
            "one past the end must be an error, not the last monitor again"
        );
    }
}
