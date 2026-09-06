//! `trix dd-probe` — the Phase 0 spike for the Desktop Duplication backend.
//!
//! **This file is throwaway.** It exists to answer one question the design in
//! `docs/superpowers/specs/2026-09-06-trix-desktop-duplication-design.md` rests
//! on and cannot answer from here: does DXGI Desktop Duplication clear the
//! yellow capture border *from a Trix process*? The evidence so far is OBS's
//! behaviour on the reporting user's machine, which is a different claim.
//!
//! It deliberately contains no sink, no seam, no encoder, no ring, and no
//! cursor. It opens a duplication, acquires and releases frames, counts them,
//! and prints. When the question is answered — either way — delete this file
//! and its subcommand in `main.rs`.
//!
//! Three things here are load-bearing for the real backend, and are why the
//! spike is worth compiling rather than just describing:
//!
//! 1. The monitor index must resolve to the adapter that *drives the output*,
//!    not to the default adapter (see [`resolve_output`]).
//! 2. `LastPresentTime` is raw QPC ticks, not the 100 ns units WGC hands us
//!    (see [`qpc_to_100ns`], the one function here worth keeping).
//! 3. The acquired texture must not outlive `ReleaseFrame`.
//!
//! # What the first run taught it
//!
//! The first version recovered from `DXGI_ERROR_ACCESS_LOST` by re-calling
//! `DuplicateOutput` on the output it had resolved at startup. On the reporting
//! user's machine that produced 37 consecutive losses and nine seconds of zero
//! frames: `DuplicateOutput` kept *succeeding* and every first `AcquireNextFrame`
//! kept failing. That is the signature of a **stale DXGI factory** — after a
//! mode change `IDXGIFactory1::IsCurrent` goes false and every adapter and
//! output it handed out is dead, so re-duplicating a cached output re-opens
//! nothing. Recovery here is now a full teardown: new factory, new adapter, new
//! output, new device, new duplication (see [`Session::open`]).
//!
//! It also learned to distrust its own result. A run where duplication was dead
//! for half its length cannot answer a question about an indicator that is only
//! painted while something is capturing, so the probe measures how long it was
//! actually live and refuses to call an unhealthy run an answer.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice,
    ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::core::Interface as _;

/// How long `AcquireNextFrame` waits before reporting that nothing changed.
///
/// Short enough that the progress line still ticks once a second on a
/// completely static desktop, long enough that a still screen does not spin
/// this loop. A timeout is not an error: like WGC, duplication only delivers
/// on change.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// First wait after an access loss, doubling up to [`MAX_BACKOFF_MS`] while
/// each rebuilt session dies without delivering anything.
///
/// A fullscreen transition is several losses in a row, so rebuilding instantly
/// turns that into a busy loop against a display that is still changing mode.
/// The backoff resets the moment a session delivers a frame, so an ordinary
/// transition costs one short pause rather than a growing one.
const BACKOFF_START_MS: u64 = 100;
// Capped low. The first alt-tab run spent its backoff at 1.5 s and got only
// fifteen attempts across twenty-three dark seconds, which is too coarse to
// tell "recovers after four seconds" from "never recovers" — and those are
// different verdicts for the product.
const MAX_BACKOFF_MS: u64 = 500;

/// Stops rebuilding rather than scrolling forever.
///
/// The real backend surfaces loss as session death and lets `run_driven_inner`
/// rebuild on its own budget; this cap exists only so a machine where
/// duplication cannot hold at all prints a verdict the user can read back.
const MAX_ACCESS_LOSSES: u32 = 300;

/// Losses reported in full before the log switches to counting them.
///
/// The first run printed 37 identical lines and buried its own numbers. The
/// first few carry the diagnosis; the rest are a count.
const LOSSES_LOGGED_IN_FULL: u32 = 5;

/// Below this share of the run spent actually capturing, the probe declines to
/// treat what the user saw as an answer.
///
/// The border is painted only while something is capturing, so "no border"
/// during a stretch where duplication was dead is not evidence of anything —
/// which is exactly how the first run went. Two thirds is a judgement call, set
/// where a reasonable person watching the screen would have had a fair chance
/// of seeing an indicator that was going to appear.
const HEALTHY_ENOUGH: f64 = 0.66;
// A run may lose a moment to a fullscreen transition and still be worth
// believing, so this must stay below 1.0. Compile-time rather than a test,
// matching `main.rs`'s constants: both sides are known at compile time, so a
// runtime assertion on them only trips clippy's `assertions_on_constants`.
const _: () = assert!(HEALTHY_ENOUGH > 0.0 && HEALTHY_ENOUGH < 1.0);

/// Converts a QPC tick count into the 100 ns units the rest of the pipeline
/// already uses.
///
/// This is the one piece of arithmetic Ship 1 inherits verbatim. WGC's
/// `SystemRelativeTime` is *already* 100 ns; Desktop Duplication's
/// `LastPresentTime` is raw QPC. The two are not interchangeable, and mixing
/// them desynchronises audio.
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
    adapter_index: u32,
    adapter_name: String,
    output_index: u32,
    output_name: String,
    width: i32,
    height: i32,
    left: i32,
    top: i32,
}

/// Walks DXGI the way `trix_core::probe` does, and stops at `monitor_index`.
///
/// The counting rule is the contract: desktop-attached outputs only, counted
/// **across** adapters in DXGI's order, because that is what
/// `config.monitor_index` stores and what the settings dropdown binds to.
/// `probe.rs` owns the test that pins this index space against
/// `windows-capture`'s. This is the same walk, repeated here rather than shared
/// because the spike needs the live COM objects and `probe.rs` deliberately
/// returns only descriptions.
///
/// Returning the *adapter* is the entire point of the function. Creating the
/// device on the default adapter instead would work on this developer machine
/// and fail with `DXGI_ERROR_UNSUPPORTED` on a hybrid-GPU laptop whose display
/// hangs off the iGPU — which is exactly the configuration the design flags as
/// its largest technical risk.
///
/// **Takes the factory rather than making one**, so that recovery can hand it a
/// fresh one. A factory that has gone stale keeps handing out adapters and
/// outputs that look valid and duplicate into nothing.
fn resolve_output(factory: &IDXGIFactory1, monitor_index: u32) -> Result<Target> {
    let mut seen = 0u32;
    let mut adapter_index = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let adapter_desc = unsafe { adapter.GetDesc1() }.context("IDXGIAdapter1::GetDesc1")?;
        let adapter_name = wide_to_string(&adapter_desc.Description);

        let mut output_index = 0u32;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            let out_desc = unsafe { output.GetDesc() }.context("IDXGIOutput::GetDesc")?;
            if out_desc.AttachedToDesktop.as_bool() {
                if seen == monitor_index {
                    let r = out_desc.DesktopCoordinates;
                    return Ok(Target {
                        adapter,
                        output,
                        adapter_index,
                        adapter_name,
                        output_index,
                        output_name: wide_to_string(&out_desc.DeviceName),
                        width: r.right - r.left,
                        height: r.bottom - r.top,
                        left: r.left,
                        top: r.top,
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

/// Creates a D3D11 device **on the given adapter**.
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
/// These three HRESULTs are worth naming because they are the difference
/// between "this machine cannot do it", "close OBS", and "you are on RDP".
/// Anything else is a genuine surprise and is reported raw.
fn duplicate(output: &IDXGIOutput, device: &ID3D11Device) -> Result<IDXGIOutputDuplication> {
    let output1: IDXGIOutput1 = output
        .cast()
        .context("this output does not support IDXGIOutput1, so it cannot be duplicated")?;

    unsafe { output1.DuplicateOutput(device) }.map_err(|e| {
        let code = e.code();
        if code == DXGI_ERROR_UNSUPPORTED {
            anyhow!(
                "DXGI_ERROR_UNSUPPORTED — the device was created on the wrong adapter for this \
                 output, or this display cannot be duplicated at all. A hybrid-GPU laptop is the \
                 usual cause."
            )
        } else if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
            anyhow!(
                "DXGI_ERROR_NOT_CURRENTLY_AVAILABLE — this output already has the maximum number \
                 of duplications open. Close OBS or any other duplication-based recorder and run \
                 this again."
            )
        } else if code == DXGI_ERROR_SESSION_DISCONNECTED {
            anyhow!(
                "DXGI_ERROR_SESSION_DISCONNECTED — there is no interactive desktop session to \
                 duplicate (a remote-desktop connection, a locked screen, or a service context)"
            )
        } else {
            anyhow!("DuplicateOutput failed: {e}")
        }
    })
}

/// One complete duplication chain, from the factory down.
///
/// Everything is owned together because everything has to be **rebuilt**
/// together. The first version of this probe held the factory, adapter and
/// output from startup and only re-made the duplication on access loss, which
/// is why it never recovered: a factory that has gone stale keeps handing out
/// objects that duplicate successfully into nothing.
struct Session {
    factory: IDXGIFactory1,
    device: ID3D11Device,
    dupl: IDXGIOutputDuplication,
    /// When this chain was built, for the healthy-time accounting.
    opened: Instant,
    /// The last moment this chain delivered a desktop frame. `None` means it
    /// never did, which is the signature the first run died on.
    last_frame: Option<Instant>,
}

impl Session {
    fn open(monitor_index: u32) -> Result<(Self, Target)> {
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1 failed")?;
        let target = resolve_output(&factory, monitor_index)?;
        let device = create_device(&target.adapter)?;
        let dupl = duplicate(&target.output, &device)?;
        Ok((Self { factory, device, dupl, opened: Instant::now(), last_frame: None }, target))
    }

    /// How long this chain was actually delivering, measured to its last frame
    /// rather than to its death. A chain that opened and immediately died
    /// contributes nothing, which is the honest accounting.
    fn live_time(&self) -> Duration {
        self.last_frame.map(|at| at - self.opened).unwrap_or_default()
    }

    /// What the diagnosis turns on, printed at the moment of loss.
    ///
    /// `IsCurrent` going false says the factory is stale and the whole chain
    /// has to be rebuilt — the finding this version exists for. A device
    /// removed reason says the GPU itself went away, which is a different
    /// problem with a different fix.
    fn explain_loss(&self) -> String {
        let current = unsafe { self.factory.IsCurrent() }.as_bool();
        let removed = match unsafe { self.device.GetDeviceRemovedReason() } {
            Ok(()) => "device fine".to_string(),
            Err(e) => format!("device removed: {}", e.code().0),
        };
        format!("factory current: {current}, {removed}, {}", foreground_note())
    }
}

/// What owns the screen at the moment access was lost.
///
/// This is the evidence that separates the two surviving explanations for a
/// duplication that opens and then refuses to deliver. If a window covering the
/// whole monitor is in front at every loss, the game is holding the output and
/// duplication is locked out for as long as it does — a property of the game,
/// not of the rebuild. If it is not, something else is invalidating it and the
/// rebuild is still the suspect.
///
/// Best-effort throughout: every failure degrades to a note rather than an
/// error, because this is a diagnostic string and must never be the reason a
/// probe run ends.
fn foreground_note() -> String {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetWindowTextW,
    };

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd == HWND::default() {
        return "no foreground window".to_string();
    }

    let mut title = [0u16; 96];
    let len = unsafe { GetWindowTextW(hwnd, &mut title) }.max(0) as usize;
    let title = String::from_utf16_lossy(&title[..len.min(title.len())]);

    let mut rect = RECT::default();
    match unsafe { GetWindowRect(hwnd, &mut rect) } {
        Ok(()) => format!(
            "front {title:?} {}x{} at ({},{})",
            rect.right - rect.left,
            rect.bottom - rect.top,
            rect.left,
            rect.top
        ),
        Err(_) => format!("front {title:?} (no rect)"),
    }
}

/// Everything the run measures.
#[derive(Default)]
struct Stats {
    /// Frames carrying new desktop content (`LastPresentTime != 0`).
    desktop: u64,
    /// Frames where only the pointer moved. The real backend skips these; they
    /// are counted here because their share is worth knowing before judging
    /// how much a missing cursor actually costs.
    mouse_only: u64,
    timeouts: u64,
    access_losses: u32,
    /// Rebuilt chains that went on to deliver at least one frame. The number
    /// that separates "recovers from a fullscreen transition" from "never
    /// comes back", which is the whole reason this version exists.
    recoveries: u32,
    /// Summed across every chain, so the run can say how much of itself was
    /// spent actually capturing.
    live: Duration,
    longest_live: Duration,
    /// The longest stretch between losing access and delivering again — the
    /// blackout a viewer of the finished clip would see as a jump cut.
    ///
    /// This is the product number, not a diagnostic one. In the real backend a
    /// blackout is a hole in the replay ring at the exact moment somebody wants
    /// to clip: a game going fullscreen is when the interesting thing happens.
    longest_blackout: Duration,
    /// When the current outage began, if there is one in progress.
    blackout_since: Option<Instant>,
    /// Gaps between consecutive `LastPresentTime` values, in 100 ns units.
    /// This is the *presented* cadence, not the loop's — the two diverge as
    /// soon as the loop is slower than the display.
    min_gap_100ns: i64,
    max_gap_100ns: i64,
    total_gap_100ns: i64,
    gaps: u64,
}

impl Stats {
    fn record_present(&mut self, previous: Option<i64>, now: i64) {
        self.desktop += 1;
        let Some(previous) = previous else { return };

        // A gap of zero or less means the clock did not advance between two
        // frames DXGI called distinct presents. That is not a cadence sample,
        // and averaging it in would flatter every number below it.
        let gap = now - previous;
        if gap <= 0 {
            return;
        }
        if self.gaps == 0 {
            self.min_gap_100ns = gap;
            self.max_gap_100ns = gap;
        } else {
            self.min_gap_100ns = self.min_gap_100ns.min(gap);
            self.max_gap_100ns = self.max_gap_100ns.max(gap);
        }
        self.total_gap_100ns += gap;
        self.gaps += 1;
    }

    fn retire(&mut self, session: &Session) {
        let live = session.live_time();
        self.live += live;
        self.longest_live = self.longest_live.max(live);
    }

    /// Ends the outage in progress and folds it into [`Self::longest_blackout`],
    /// returning how long it lasted. Zero when nothing was dark.
    fn close_blackout(&mut self) -> Duration {
        let Some(since) = self.blackout_since.take() else {
            return Duration::ZERO;
        };
        let dark = since.elapsed();
        self.longest_blackout = self.longest_blackout.max(dark);
        dark
    }
}

fn ms(hundred_ns: i64) -> f64 {
    hundred_ns as f64 / 10_000.0
}

/// Names the handful of formats duplication actually hands back.
///
/// The output of this probe is read by someone who is not looking at this
/// source, so a bare `87` is not a useful thing to send them. 87 is
/// `B8G8R8A8_UNORM` — the same format WGC delivers and `VideoConverter`
/// already consumes, which is the answer worth being able to read at a glance.
/// A run that switches to one of the float formats mid-way has had the display
/// put into HDR under it, which is its own explanation for a lost duplication.
fn format_name(format: i32) -> &'static str {
    match format {
        87 => "B8G8R8A8_UNORM (same as WGC)",
        28 => "R8G8B8A8_UNORM",
        24 => "R10G10B10A2_UNORM (HDR / wide colour)",
        10 => "R16G16B16A16_FLOAT (HDR)",
        _ => "unrecognised — worth reporting",
    }
}

/// Counts the user into position before anything starts capturing.
///
/// The first run's fatal flaw was not technical. The probe began the moment the
/// batch file opened, the user spent the first ten seconds alt-tabbing, and by
/// the time they were looking at the game the duplication was already dead — so
/// "no border" described a period when nothing was capturing at all. Nothing
/// is opened until this returns.
fn count_in(seconds: u64) {
    if seconds == 0 {
        return;
    }
    println!("\n[get ready]");
    println!("  ALT-TAB INTO YOUR GAME NOW. Nothing is being captured yet.");
    for remaining in (1..=seconds).rev() {
        println!("  starting in {remaining}...");
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub fn run(monitor_index: u32, seconds: u64, warmup: u64) -> Result<()> {
    println!("trix dd-probe — Desktop Duplication spike");
    println!("=========================================");

    let mut qpf = 0i64;
    unsafe { QueryPerformanceFrequency(&mut qpf) }.context("QueryPerformanceFrequency failed")?;

    count_in(warmup);

    let (mut session, target) = Session::open(monitor_index)?;
    println!("\n[target]");
    println!(
        "  monitor {monitor_index}: {} — {}x{} at ({}, {})",
        target.output_name, target.width, target.height, target.left, target.top
    );
    println!(
        "  driven by adapter {} ({}), output {}",
        target.adapter_index, target.adapter_name, target.output_index
    );
    // Nothing below this line needs the adapter or the output, and holding COM
    // references to a monitor for the length of the run is another thing that
    // could be blamed when a rebuild misbehaves. Let them go here so the
    // session is the only thing alive that touches DXGI.
    drop(target);
    println!("  QPC frequency: {qpf} Hz");

    println!("\n[duplication]");
    println!("  opened OK — capturing for {seconds} s");
    // Printed here as well as at every loss, for two reasons: it confirms the
    // tester actually alt-tabbed into the game during the countdown rather than
    // watching this window, and it means the one line the diagnosis may hang on
    // is exercised on every run instead of only on the runs that go wrong.
    println!("  {}", foreground_note());
    println!("  STAY IN THE GAME and watch for a yellow border around the screen.");
    println!();

    let mut stats = Stats::default();
    let mut described = false;
    let mut previous_present: Option<i64> = None;
    let mut backoff_ms = BACKOFF_START_MS;
    let started = Instant::now();
    let run_for = Duration::from_secs(seconds);
    let mut last_report = started;
    // The first chain was opened before the header was printed, so its own
    // clock starts earlier than the run's. Left alone, its live time counts
    // that header against a run that had not begun, and the headline share
    // comes out above 100%. Both clocks start here instead.
    session.opened = started;

    while started.elapsed() < run_for {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let acquired =
            unsafe { session.dupl.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource) };

        match acquired {
            Ok(()) => {
                if let Some(resource) = &resource
                    && !described
                {
                    // Cast, read, and finish with the texture entirely inside
                    // this arm. It belongs to DXGI until `ReleaseFrame` below,
                    // and the real backend's borrowed `SourceFrame<'_>` exists
                    // to say precisely that.
                    let texture: ID3D11Texture2D =
                        resource.cast().context("the acquired frame is not an ID3D11Texture2D")?;
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    unsafe { texture.GetDesc(&mut desc) };
                    println!(
                        "  surface: {}x{}, DXGI_FORMAT {} — {}",
                        desc.Width,
                        desc.Height,
                        desc.Format.0,
                        format_name(desc.Format.0)
                    );
                    described = true;
                }

                if info.LastPresentTime == 0 {
                    stats.mouse_only += 1;
                } else {
                    let present = qpc_to_100ns(info.LastPresentTime, qpf);
                    stats.record_present(previous_present, present);
                    previous_present = Some(present);

                    if session.last_frame.is_none() && stats.access_losses > 0 {
                        stats.recoveries += 1;
                        backoff_ms = BACKOFF_START_MS;
                        let blackout = stats.close_blackout();
                        println!(
                            "  [{:>5.1}s] recovered after {:.1} s dark",
                            started.elapsed().as_secs_f64(),
                            blackout.as_secs_f64()
                        );
                    }
                    session.last_frame = Some(Instant::now());
                }

                unsafe { session.dupl.ReleaseFrame() }.context("ReleaseFrame failed")?;
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => stats.timeouts += 1,
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                stats.access_losses += 1;
                stats.retire(&session);
                // `get_or_insert`, not a fresh stamp: a fullscreen transition
                // is a burst of losses, and that is one blackout as far as the
                // recording is concerned, not six.
                stats.blackout_since.get_or_insert_with(Instant::now);

                // The first few carry the diagnosis, and then every tenth keeps
                // a heartbeat going through a long outage — the foreground
                // window in those lines is how we see whether the screen
                // changed under it while it was dark.
                if stats.access_losses <= LOSSES_LOGGED_IN_FULL || stats.access_losses % 10 == 0 {
                    println!(
                        "  [{:>5.1}s] access lost (#{}) — {}; rebuilding the whole chain",
                        started.elapsed().as_secs_f64(),
                        stats.access_losses,
                        session.explain_loss()
                    );
                    if stats.access_losses == LOSSES_LOGGED_IN_FULL {
                        println!("  (from here only every tenth loss is printed)");
                    }
                }
                if stats.access_losses >= MAX_ACCESS_LOSSES {
                    println!(
                        "  [{:>5.1}s] giving up after {MAX_ACCESS_LOSSES} losses — duplication \
                         cannot hold on this machine in this state",
                        started.elapsed().as_secs_f64()
                    );
                    break;
                }

                std::thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);

                // **Release the dead chain before building its replacement.**
                // Creating the new duplication first left two duplications of
                // the same output alive in this process, and DXGI hands the
                // second one back looking valid while every `AcquireNextFrame`
                // on it fails — which is exactly the 18-losses-0-recoveries
                // signature this arm could not explain. The rebuild is only a
                // rebuild once the old one is actually gone.
                drop(session);

                let (rebuilt, _) = Session::open(monitor_index)
                    .context("could not rebuild the duplication after access loss")?;
                session = rebuilt;
                previous_present = None;
                described = false;
            }
            Err(e) if e.code() == DXGI_ERROR_DEVICE_REMOVED => bail!(
                "DXGI_ERROR_DEVICE_REMOVED — the graphics device was reset or the driver \
                 restarted. The real backend would recover by rebuilding the whole session."
            ),
            Err(e) => return Err(e).context("AcquireNextFrame failed"),
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            println!(
                "  [{:>5.1}s] desktop {}  mouse-only {}  timeouts {}  losses {}",
                started.elapsed().as_secs_f64(),
                stats.desktop,
                stats.mouse_only,
                stats.timeouts,
                stats.access_losses,
            );
        }
    }
    stats.retire(&session);
    // A run that ends while still dark has a blackout with no known end, so its
    // length is a floor rather than a measurement. Folded in anyway — an
    // outage that outlasted the run is the worst one by definition — but
    // remembered separately, because "never came back" is a different verdict
    // from "came back slowly".
    let ended_dark = stats.blackout_since.is_some();
    stats.close_blackout();

    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    let live = stats.live.as_secs_f64();
    let share = live / elapsed;

    println!("\n[result]");
    println!("  ran for              {elapsed:.1} s");
    println!("  actually capturing   {live:.1} s  ({:.0}% of the run)", share * 100.0);
    println!("  longest unbroken     {:.1} s", stats.longest_live.as_secs_f64());
    // Per *live* second, not per second of the run: a run that spent half its
    // length dead would otherwise report half the frame rate it delivered.
    let fps = if live > 0.0 { stats.desktop as f64 / live } else { 0.0 };
    println!("  desktop frames       {}  ({fps:.1} /s while live)", stats.desktop);
    println!("  mouse-only frames    {}", stats.mouse_only);
    println!("  wait timeouts        {}", stats.timeouts);
    println!("  access losses        {}  ({} recovered)", stats.access_losses, stats.recoveries);
    if stats.gaps > 0 {
        println!(
            "  present gap          min {:.1} ms  avg {:.1} ms  max {:.1} ms",
            ms(stats.min_gap_100ns),
            ms(stats.total_gap_100ns / stats.gaps as i64),
            ms(stats.max_gap_100ns),
        );
    } else {
        println!("  present gap          no two presents to compare");
    }

    // Only printed when there was something to recover from, so an ordinary
    // run does not carry a section about a situation it never met.
    if stats.access_losses > 0 {
        println!("\n[recovery]");
        println!(
            "  {} loss(es), {} recovered, worst blackout {:.1} s{}",
            stats.access_losses,
            stats.recoveries,
            stats.longest_blackout.as_secs_f64(),
            if ended_dark { " (and it never came back)" } else { "" },
        );
        if ended_dark {
            println!("  NEVER RECOVERED — this is the failure the backend cannot ship with.");
        } else if stats.longest_blackout > Duration::from_secs(3) {
            println!("  Recovered, but slowly. A blackout that long is a hole in the clip.");
        } else {
            println!("  Recovered every time, and quickly. This is the behaviour we want.");
        }
    }

    println!("\n[the question]");
    if stats.desktop == 0 {
        println!("  NO frames arrived at all. Whatever was on screen, this run cannot answer");
        println!("  the question — nothing was ever being captured. Please send this log.");
    } else if share < HEALTHY_ENOUGH {
        println!("  INCONCLUSIVE. Duplication was only live for {live:.1} s of {elapsed:.1} s,");
        println!("  and Windows only draws the border while something is capturing — so what");
        println!("  you saw in the gaps means nothing either way.");
        println!("  Please send this log and run it once more.");
    } else {
        println!(
            "  Duplication held for {:.0}% of the run, so this IS a valid test.",
            share * 100.0
        );
        println!("  Was there a yellow border around the screen while it ran?");
        println!("    no border  -> the Desktop Duplication backend is worth building");
        println!("    border     -> it is not, and this design stops here");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion Ship 1 inherits. 10 MHz is the QPC frequency on
    /// essentially every machine this will run on, which makes the numbers
    /// checkable by eye: at 10 MHz one tick already *is* 100 ns.
    #[test]
    fn qpc_ticks_convert_to_hundred_nanosecond_units() {
        assert_eq!(qpc_to_100ns(0, 10_000_000), 0);
        assert_eq!(qpc_to_100ns(1, 10_000_000), 1);
        assert_eq!(qpc_to_100ns(10_000_000, 10_000_000), 10_000_000, "one second");

        // A 3.579545 MHz timer — the old ACPI power-management clock, and the
        // reason this cannot just assume 10 MHz and multiply.
        assert_eq!(qpc_to_100ns(3_579_545, 3_579_545), 10_000_000, "one second");
    }

    /// The reason for the 128-bit intermediate, stated as a test so that
    /// anyone who "simplifies" it back to `i64` finds out immediately.
    ///
    /// `LastPresentTime` counts from boot, so a week of uptime on a 10 MHz
    /// timer is 6.0e12 ticks — and `6.0e12 * 10_000_000` is far past
    /// `i64::MAX`. In `i64` that wraps to a negative timestamp on any machine
    /// that has not been rebooted recently.
    #[test]
    fn a_week_of_uptime_does_not_overflow() {
        const QPF: i64 = 10_000_000;
        let week = 7 * 24 * 60 * 60 * QPF;

        let converted = qpc_to_100ns(week, QPF);
        assert_eq!(converted, week, "at 10 MHz a tick is already 100 ns");
        assert!(converted > 0, "must not wrap negative");

        // The overflow this guards against, made explicit: the naive
        // expression from the design document does wrap at this scale.
        assert!(week.checked_mul(10_000_000).is_none(), "i64 arithmetic overflows here");
    }

    /// Gaps are the presented cadence, so a repeated `LastPresentTime` — which
    /// DXGI does hand out — must not count as a zero-millisecond frame and
    /// flatter the average.
    #[test]
    fn repeated_present_times_are_not_cadence_samples() {
        let mut stats = Stats::default();
        stats.record_present(None, 1_000);
        stats.record_present(Some(1_000), 1_000);
        assert_eq!(stats.desktop, 2, "both are frames");
        assert_eq!(stats.gaps, 0, "neither is a cadence sample");
    }

    /// The first sample has to seed both extremes. Leaving them at
    /// `Default::default()` would peg the minimum at zero forever and report a
    /// 0.0 ms best-case gap on every run.
    #[test]
    fn the_first_gap_seeds_both_extremes() {
        let mut stats = Stats::default();
        stats.record_present(None, 0);
        stats.record_present(Some(0), 170_000);
        assert_eq!(stats.gaps, 1);
        assert_eq!(stats.min_gap_100ns, 170_000);
        assert_eq!(stats.max_gap_100ns, 170_000);

        stats.record_present(Some(170_000), 250_000);
        assert_eq!(stats.min_gap_100ns, 80_000, "the smaller gap must win");
        assert_eq!(stats.max_gap_100ns, 170_000);
    }

    /// A fullscreen transition arrives as a burst of losses, and the recording
    /// loses one continuous stretch, not one per loss. Stamping the clock on
    /// every loss would report a blackout of a few milliseconds for an outage
    /// that actually cost seconds — flattering exactly the number the backend
    /// will be judged on.
    #[test]
    fn a_burst_of_losses_is_a_single_blackout() {
        let mut stats = Stats::default();
        let first_loss = Instant::now() - Duration::from_millis(400);

        stats.blackout_since.get_or_insert(first_loss);
        stats.blackout_since.get_or_insert(Instant::now());
        assert_eq!(stats.blackout_since, Some(first_loss), "a later loss must not restart it");

        let dark = stats.close_blackout();
        assert!(dark >= Duration::from_millis(400), "measured from the first loss, not the last");
        assert_eq!(stats.longest_blackout, dark);
        assert_eq!(stats.close_blackout(), Duration::ZERO, "nothing is dark now");
    }

    /// The report quotes the worst outage, so a short recovery after a long one
    /// must not overwrite it.
    #[test]
    fn the_worst_blackout_is_the_one_reported() {
        let mut stats = Stats {
            blackout_since: Some(Instant::now() - Duration::from_millis(900)),
            ..Default::default()
        };
        let long = stats.close_blackout();

        stats.blackout_since = Some(Instant::now());
        stats.close_blackout();

        assert_eq!(stats.longest_blackout, long, "the brief second outage must not win");
    }

    /// The threshold that decides whether a run is allowed to be an answer.
    ///
    /// The first run on the reporting user's machine was live for 11 s of 20 —
    /// 55% — and the user was only watching the game during the dead half. A
    /// probe that called that "no border" would have sent the project down a
    /// week of work on a result that measured nothing, so the bar has to sit
    /// above it.
    #[test]
    fn the_first_runs_health_would_not_have_counted_as_an_answer() {
        let share = 11.0 / 20.0;
        assert!(share < HEALTHY_ENOUGH, "55% live must be rejected as inconclusive");
    }
}
