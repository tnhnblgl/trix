//! Windows.Graphics.Capture video capture (Phase 1).
//!
//! Provides the two probe entry points that prove the capture stack works:
//! [`snapshot`] saves one frame as PNG, [`measure`] reports frame-arrival
//! cadence from the WGC timestamps (QPC-based, 100 ns units) that the
//! encoder will later use for A/V sync.

use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    encoder::ImageFormat,
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

/// What the probe capture session does with arriving frames.
enum Mode {
    /// Save the first frame as PNG, report on the channel, then stop.
    Snapshot { path: PathBuf, done: Sender<Result<()>> },
    /// Accumulate arrival statistics until externally stopped.
    Measure,
}

struct Flags {
    mode: Mode,
}

struct Stats {
    frames: u64,
    first_qpc_100ns: Option<i64>,
    last_qpc_100ns: Option<i64>,
    min_delta_100ns: i64,
    max_delta_100ns: i64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            frames: 0,
            first_qpc_100ns: None,
            last_qpc_100ns: None,
            min_delta_100ns: i64::MAX,
            max_delta_100ns: 0,
        }
    }
}

struct ProbeCapture {
    mode: Mode,
    stats: Stats,
    announced: bool,
}

impl GraphicsCaptureApiHandler for ProbeCapture {
    type Flags = Flags;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        Ok(Self { mode: ctx.flags.mode, stats: Stats::default(), announced: false })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<()> {
        if !self.announced {
            self.announced = true;
            tracing::info!(
                width = frame.width(),
                height = frame.height(),
                format = ?frame.color_format(),
                "first frame arrived"
            );
        }

        if let Ok(timestamp) = frame.timestamp() {
            let qpc = timestamp.Duration;
            let stats = &mut self.stats;
            stats.frames += 1;
            if stats.first_qpc_100ns.is_none() {
                stats.first_qpc_100ns = Some(qpc);
            }
            if let Some(last) = stats.last_qpc_100ns {
                let delta = qpc - last;
                stats.min_delta_100ns = stats.min_delta_100ns.min(delta);
                stats.max_delta_100ns = stats.max_delta_100ns.max(delta);
            }
            stats.last_qpc_100ns = Some(qpc);
        }

        if let Mode::Snapshot { path, done } = &self.mode {
            let saved = frame
                .save_as_image(path, ImageFormat::Png)
                .map_err(|e| anyhow!("failed to save snapshot: {e}"));
            let _ = done.send(saved);
            capture_control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        tracing::debug!("capture item closed");
        Ok(())
    }
}

/// Maps our zero-based config index to the crate's one-based index.
fn monitor_for(index: u32) -> Result<Monitor> {
    Monitor::from_index(index as usize + 1)
        .map_err(|e| anyhow!("monitor {index} not available: {e}"))
}

fn settings(monitor: Monitor, mode: Mode) -> Settings<Flags, Monitor> {
    Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        Flags { mode },
    )
}

/// Captures a single frame of `monitor_index` and writes it to `path` as PNG.
pub fn snapshot(monitor_index: u32, path: PathBuf) -> Result<()> {
    let monitor = monitor_for(monitor_index)?;
    let (done, result) = mpsc::channel();

    let control = ProbeCapture::start_free_threaded(settings(
        monitor,
        Mode::Snapshot { path: path.clone(), done },
    ))
    .map_err(|e| anyhow!("failed to start capture: {e}"))?;

    let outcome = result.recv_timeout(Duration::from_secs(5));
    control.stop().map_err(|e| anyhow!("failed to stop capture: {e}"))?;

    match outcome {
        Ok(saved) => {
            saved?;
            println!("snapshot saved to {}", path.display());
            Ok(())
        }
        Err(_) => bail!("no frame arrived within 5 s"),
    }
}

/// Captures `monitor_index` for `seconds` and reports frame-arrival cadence.
///
/// WGC only delivers frames when screen content changes, so a static desktop
/// reads low; run moving content (a video/game) to see the full refresh rate.
pub fn measure(monitor_index: u32, seconds: u64) -> Result<()> {
    let monitor = monitor_for(monitor_index)?;
    let refresh = monitor.refresh_rate().unwrap_or(0);
    println!(
        "measuring frame arrival for {seconds} s (monitor refresh: {refresh} Hz) — \
         WGC only delivers frames when content changes, so keep something moving on screen"
    );

    let control = ProbeCapture::start_free_threaded(settings(monitor, Mode::Measure))
        .map_err(|e| anyhow!("failed to start capture: {e}"))?;

    std::thread::sleep(Duration::from_secs(seconds));

    let handler = control.callback();
    control.stop().map_err(|e| anyhow!("failed to stop capture: {e}"))?;
    let handler = handler.lock();
    let stats = &handler.stats;

    let span_100ns = match (stats.first_qpc_100ns, stats.last_qpc_100ns) {
        (Some(first), Some(last)) if last > first => last - first,
        _ => bail!("fewer than two frames arrived in {seconds} s — is the screen fully static?"),
    };
    let span_secs = span_100ns as f64 / 10_000_000.0;
    let fps = (stats.frames.saturating_sub(1)) as f64 / span_secs;

    println!("\n[capture cadence]");
    println!("  frames:         {}", stats.frames);
    println!("  active span:    {:.2} s", span_secs);
    println!("  effective rate: {:.1} fps", fps);
    println!(
        "  frame delta:    min {:.2} ms / max {:.2} ms",
        stats.min_delta_100ns as f64 / 10_000.0,
        stats.max_delta_100ns as f64 / 10_000.0,
    );
    Ok(())
}
