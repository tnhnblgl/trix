//! Windows Graphics Capture, behind the [`FrameSink`] seam.
//!
//! Everything WGC-specific lives here and nowhere else: the `Settings`
//! builder, the border and update-interval negotiation, the one-based monitor
//! index the crate wants, and the adapter that turns a `GraphicsCaptureApiHandler`
//! callback into a [`SourceFrame`]. No capture logic moved when the seam was
//! introduced — the adapter is generic over the sink, so all three sinks kept
//! their bodies and lost only their imports.

use std::sync::mpsc::{self, Sender};

use anyhow::{Result, anyhow};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, MinimumUpdateIntervalSettings,
        SecondaryWindowSettings, Settings,
    },
};

use super::source::{Flow, FrameSink, MonitorInfo, SinkRef, SourceFrame};

/// A running WGC session. Owns the capture thread; dropping it without
/// `stop` leaves that thread running, exactly as `CaptureControl` does.
pub(super) struct Session<S: FrameSink> {
    control: CaptureControl<Adapter<S>, anyhow::Error>,
}

impl<S: FrameSink> Session<S> {
    pub(super) fn is_finished(&self) -> bool {
        self.control.is_finished()
    }

    /// Errors carry no context of their own: the call sites already word this
    /// ("failed to stop capture", "capture session failed") and had that
    /// wording before the seam.
    pub(super) fn stop(self) -> Result<()> {
        self.control.stop().map_err(|e| anyhow!("{e}"))
    }

    pub(super) fn wait(self) -> Result<()> {
        self.control.wait().map_err(|e| anyhow!("{e}"))
    }
}

/// What `start` hands the capture thread: the sink's own flags, plus the
/// channel the built sink comes back on.
struct AdapterFlags<S: FrameSink> {
    flags: S::Flags,
    publish: Sender<SinkRef<S>>,
}

/// Feeds a [`FrameSink`] from a WGC session.
struct Adapter<S: FrameSink> {
    sink: SinkRef<S>,
}

impl<S: FrameSink> GraphicsCaptureApiHandler for Adapter<S> {
    type Flags = AdapterFlags<S>;
    type Error = anyhow::Error;

    fn new(ctx: Context<Self::Flags>) -> Result<Self> {
        let AdapterFlags { flags, publish } = ctx.flags;
        let sink = SinkRef::new(S::new(&ctx.device, flags)?);
        // `start_free_threaded` does not return until this has run, so the
        // receiver is always still alive here. Swallowed rather than
        // unwrapped anyway: a send failure means the caller is already gone,
        // and panicking on the capture thread would be a worse answer than
        // capturing into a sink nobody reads.
        let _ = publish.send(sink.clone());
        Ok(Self { sink })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<()> {
        // A frame we cannot place on the timeline is a frame we cannot
        // encode: the sinks pace, seek and sync entirely by this value.
        let qpc_100ns = match frame.timestamp() {
            Ok(t) => t.Duration,
            Err(e) => {
                tracing::warn!("frame without timestamp, skipped: {e}");
                return Ok(());
            }
        };
        let flow = self.sink.lock().on_frame(SourceFrame {
            texture: frame.as_raw_texture(),
            qpc_100ns,
            width: frame.width(),
            height: frame.height(),
        })?;
        if flow == Flow::Stop {
            control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<()> {
        self.sink.lock().on_closed()
    }
}

/// Resolves Trix's zero-based monitor index to the crate's one-based one.
fn monitor_for(monitor_index: u32) -> Result<Monitor> {
    Monitor::from_index(monitor_index as usize + 1)
        .map_err(|e| anyhow!("monitor {monitor_index} not available: {e}"))
}

pub(super) fn monitor_info(monitor_index: u32) -> Result<MonitorInfo> {
    let monitor = monitor_for(monitor_index)?;
    Ok(MonitorInfo {
        width: monitor.width().map_err(|e| anyhow!("monitor width: {e}"))?,
        height: monitor.height().map_err(|e| anyhow!("monitor height: {e}"))?,
        refresh_hz: monitor.refresh_rate().unwrap_or(0),
    })
}

pub(super) fn start<S: FrameSink>(
    monitor_index: u32,
    pace_to_fps: Option<u32>,
    flags: S::Flags,
) -> Result<(Session<S>, SinkRef<S>)> {
    let monitor = monitor_for(monitor_index)?;
    let (publish, published) = mpsc::channel();
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        super::border_settings(),
        SecondaryWindowSettings::Default,
        match pace_to_fps {
            Some(fps) => super::min_update_interval(fps),
            None => MinimumUpdateIntervalSettings::Default,
        },
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        AdapterFlags { flags, publish },
    );
    let control = Adapter::<S>::start_free_threaded(settings)
        .map_err(|e| anyhow!("failed to start capture: {e}"))?;
    // Sent from `Adapter::new`, which has already run and succeeded by the
    // time `start_free_threaded` returns.
    let sink = published
        .recv()
        .map_err(|_| anyhow!("the capture thread started without publishing its sink"))?;
    Ok((Session { control }, sink))
}
