//! The capture probe (Phase 1).
//!
//! Provides the two entry points that prove the capture stack works:
//! [`snapshot`] saves one frame as PNG, [`measure`] reports frame-arrival
//! cadence from the capture timestamps (QPC-based, 100 ns units) that the
//! encoder will later use for A/V sync.
//!
//! It runs on whichever backend [`crate::capture::source`] starts, which is
//! the point: a probe wired to one backend can only ever tell you about that
//! backend.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM};

use super::{
    source::{self, Flow, FrameSink, SourceFrame},
    stage::stage_bgra,
};
use crate::thumb;

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

impl FrameSink for ProbeCapture {
    type Flags = Flags;

    fn new(_device: &ID3D11Device, flags: Flags) -> Result<Self> {
        Ok(Self { mode: flags.mode, stats: Stats::default(), announced: false })
    }

    fn on_frame(&mut self, frame: SourceFrame<'_>) -> Result<Flow> {
        if !self.announced {
            self.announced = true;
            tracing::info!(
                width = frame.width,
                height = frame.height,
                format = format_name(texture_format(frame.texture)),
                "first frame arrived"
            );
        }

        let qpc = frame.qpc_100ns;
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

        if let Mode::Snapshot { path, done } = &self.mode {
            let saved = write_png(frame.texture, path).context("failed to save snapshot");
            let _ = done.send(saved);
            return Ok(Flow::Stop);
        }
        Ok(Flow::Continue)
    }

    fn on_closed(&mut self) -> Result<()> {
        tracing::debug!("capture item closed");
        Ok(())
    }
}

fn texture_format(texture: &ID3D11Texture2D) -> DXGI_FORMAT {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    desc.Format
}

/// Names the one surface format the encoder path accepts, and prints anything
/// else as its raw DXGI value — which is what you would look up anyway, and is
/// the thing worth seeing when a new backend hands us something unexpected.
fn format_name(format: DXGI_FORMAT) -> String {
    if format == DXGI_FORMAT_B8G8R8A8_UNORM {
        "B8G8R8A8_UNORM".to_string()
    } else {
        format!("DXGI format {}", format.0)
    }
}

/// Copies one frame to CPU memory and writes it out losslessly.
///
/// PNG, not JPEG: this image exists to be inspected when something about the
/// capture looks wrong, and a lossy copy would put the encoder's artefacts in
/// the same picture as the ones under investigation.
fn write_png(texture: &ID3D11Texture2D, path: &Path) -> Result<()> {
    let staged = stage_bgra(texture)?;
    let png = thumb::encode_png(&staged.bgra, staged.width, staged.height, staged.stride)?;
    std::fs::write(path, png).with_context(|| format!("writing {}", path.display()))
}

/// Captures a single frame of `monitor_index` and writes it to `path` as PNG.
pub fn snapshot(monitor_index: u32, path: PathBuf) -> Result<()> {
    let (done, result) = mpsc::channel();

    let control = source::start::<ProbeCapture>(
        monitor_index,
        None,
        Flags { mode: Mode::Snapshot { path: path.clone(), done } },
    )?;

    let outcome = result.recv_timeout(Duration::from_secs(5));
    control.stop().context("failed to stop capture")?;

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
/// Frames only arrive when screen content changes, so a static desktop reads
/// low; run moving content (a video/game) to see the full refresh rate.
pub fn measure(monitor_index: u32, seconds: u64) -> Result<()> {
    let refresh = source::monitor_info(monitor_index)?.refresh_hz;
    println!(
        "measuring frame arrival for {seconds} s (monitor refresh: {refresh} Hz) — \
         frames only arrive when content changes, so keep something moving on screen"
    );

    // No pacing hint: the point of this probe is the backend's true delivery
    // cadence, and asking it to slow down would measure the request.
    let control =
        source::start::<ProbeCapture>(monitor_index, None, Flags { mode: Mode::Measure })?;

    std::thread::sleep(Duration::from_secs(seconds));

    let handler = control.sink();
    control.stop().context("failed to stop capture")?;
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
