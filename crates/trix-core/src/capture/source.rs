//! The capture seam: one frame, however it was taken off the screen.
//!
//! Trix has one way of getting frames today — Windows Graphics Capture — and
//! is getting a second, DXGI Desktop Duplication, because WGC cannot always
//! suppress the yellow capture border Windows paints over a game. See
//! `docs/superpowers/specs/2026-09-06-trix-desktop-duplication-design.md`.
//!
//! The three things that consume frames — the replay ring, the direct
//! recorder, and the capture probe — used to implement `windows-capture`'s
//! [`GraphicsCaptureApiHandler`] directly, which welded all three to that one
//! API. What they actually need is much smaller than that trait: a device to
//! build their encoder on, and then a texture, a timestamp and a size per
//! frame. That smaller thing is [`FrameSink`], and it is all this module
//! exposes.
//!
//! **The texture is borrowed, and the borrow is the contract.** Desktop
//! Duplication invalidates the frame surface at `ReleaseFrame`, which happens
//! the moment [`FrameSink::on_frame`] returns — so a sink must finish with the
//! texture before it returns, and may not stash it. `SourceFrame<'_>` says
//! exactly that in the type system, and every sink here already obeys it:
//! `VideoConverter::convert` blits into its own pool, and the thumbnail path
//! copies to CPU memory. Nothing retains the texture, and nothing may start.
//!
//! [`GraphicsCaptureApiHandler`]: windows_capture::capture::GraphicsCaptureApiHandler

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

use super::{duplication, wgc};
use crate::config::CaptureMethod;

/// One captured frame, borrowed for the length of the callback.
pub struct SourceFrame<'a> {
    /// The desktop image. Valid only until [`FrameSink::on_frame`] returns.
    pub texture: &'a ID3D11Texture2D,
    /// When the frame was presented, in 100 ns units on the QPC timeline —
    /// the same clock the audio mixer's `t0` and the encoder's `pts` use.
    ///
    /// WGC reports this directly; Desktop Duplication reports raw QPC ticks
    /// and converts at its own boundary, so a sink never has to ask which
    /// backend it is on.
    pub qpc_100ns: i64,
    pub width: u32,
    pub height: u32,
}

/// What the backend should do after a frame — the seam's replacement for
/// `windows-capture`'s `InternalCaptureControl::stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep capturing.
    Continue,
    /// Close the session. The sink has what it came for, or has failed in a
    /// way that makes carrying on pointless.
    Stop,
}

/// Something that consumes captured frames, with no knowledge of where they
/// came from.
pub trait FrameSink: Send + Sized + 'static {
    /// Everything the sink needs that is not the device: output paths, encoder
    /// settings, channels back to the caller.
    type Flags: Send;

    /// Builds the sink on the capture device.
    ///
    /// The device is the backend's, and every texture the sink will be handed
    /// belongs to it — so anything that touches those textures (the NV12
    /// converter, the encoder) has to be built here rather than beforehand.
    fn new(device: &ID3D11Device, flags: Self::Flags) -> Result<Self>;

    /// Handles one frame. Returning `Err` ends the session and surfaces the
    /// error to whoever is holding the [`CaptureHandle`].
    fn on_frame(&mut self, frame: SourceFrame<'_>) -> Result<Flow>;

    /// Called once when the capture item goes away.
    fn on_closed(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A shared handle to a running sink, held by the backend's capture thread and
/// by whoever started it.
///
/// Locking never fails here. A sink that panicked mid-frame poisons the mutex,
/// and the control loop still has to be able to read its counters, stop the
/// session and finalize the file it was writing — refusing the lock at that
/// point turns one bad frame into a lost recording. The poison is recovered
/// rather than propagated, which is also what `windows-capture` did before the
/// seam existed.
pub struct SinkRef<S>(Arc<Mutex<S>>);

impl<S> Clone for SinkRef<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S> SinkRef<S> {
    pub(super) fn new(sink: S) -> Self {
        Self(Arc::new(Mutex::new(sink)))
    }

    /// Borrows the sink, recovering from a poisoned lock.
    pub fn lock(&self) -> MutexGuard<'_, S> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The running backend. The only place in the codebase that knows there is
/// more than one way to get a frame.
enum Backend<S: FrameSink> {
    Wgc(wgc::Session<S>),
    Duplication(duplication::Session<S>),
}

/// A running capture session and the sink it is feeding.
///
/// Mirrors the parts of `windows-capture`'s `CaptureControl` that the call
/// sites actually used, so switching to the seam changed their names and not
/// their shape — and adding a second backend changed nothing at all above it.
pub struct CaptureHandle<S: FrameSink> {
    backend: Backend<S>,
    sink: SinkRef<S>,
}

impl<S: FrameSink> CaptureHandle<S> {
    /// A handle to the sink, for reading its counters or asking it for a
    /// thumbnail from outside the capture thread.
    pub fn sink(&self) -> SinkRef<S> {
        self.sink.clone()
    }

    /// Whether the capture thread has ended on its own — which, for the replay
    /// engine, is the signal to rebuild.
    pub fn is_finished(&self) -> bool {
        match &self.backend {
            Backend::Wgc(session) => session.is_finished(),
            Backend::Duplication(session) => session.is_finished(),
        }
    }

    /// Asks the session to stop and waits for its thread.
    pub fn stop(self) -> Result<()> {
        match self.backend {
            Backend::Wgc(session) => session.stop(),
            Backend::Duplication(session) => session.stop(),
        }
    }

    /// Waits for a session that is already ending, and reports why it ended.
    pub fn wait(self) -> Result<()> {
        match self.backend {
            Backend::Wgc(session) => session.wait(),
            Backend::Duplication(session) => session.wait(),
        }
    }
}

/// The physical size and refresh rate of one monitor.
pub struct MonitorInfo {
    /// Physical pixels — capture frames arrive in native resolution, not
    /// DPI-scaled.
    pub width: u32,
    pub height: u32,
    /// 0 when the display does not report one.
    pub refresh_hz: u32,
}

/// Looks up `monitor_index`, for sizing an encoder before capture starts.
///
/// The index is Trix's own zero-based `monitor_index` from `config.toml`;
/// translating it is the backend's problem, not every caller's.
///
/// Deliberately **not** per-backend, even though Desktop Duplication
/// enumerates monitors itself. DXGI reports the DPI-virtualised desktop
/// rectangle — a 1920x1200 screen at 125% enumerates as 1536x960 — while both
/// backends deliver real pixels, so sizing an encoder from DXGI would record
/// every scaled display at the wrong resolution. `EnumDisplaySettingsW`, which
/// is what this reads, reports the display mode. Same reasoning, and the same
/// fix, as `probe::monitors_true_pixels`.
pub fn monitor_info(monitor_index: u32) -> Result<MonitorInfo> {
    wgc::monitor_info(monitor_index)
}

/// Starts capturing `monitor_index` into a new sink.
///
/// `pace_to_fps` asks the backend not to deliver frames much faster than that
/// rate where it can — WGC's `MinUpdateInterval`, which stops DWM copying 165
/// frames a second for an encoder that wants 60. `None` asks for every frame
/// the compositor produces, which is what the cadence probe measures. It is a
/// hint, but not an optional one: Desktop Duplication has no OS-side
/// equivalent and makes the same cut in its own loop, because a backend that
/// delivers everything the desktop produces swamps the encoder rather than
/// merely outrunning it.
pub fn start<S: FrameSink>(
    method: CaptureMethod,
    monitor_index: u32,
    pace_to_fps: Option<u32>,
    flags: S::Flags,
) -> Result<CaptureHandle<S>> {
    match method {
        CaptureMethod::Duplication => {
            let (session, sink) = duplication::start::<S>(monitor_index, pace_to_fps, flags)?;
            Ok(CaptureHandle { backend: Backend::Duplication(session), sink })
        }
        // `Auto` is Windows Graphics Capture in this release, deliberately: a
        // capture backend earns its way to being everyone's default by holding
        // up in real use first, and nobody's recording should change because
        // they upgraded.
        CaptureMethod::Auto | CaptureMethod::Wgc => {
            let (session, sink) = wgc::start::<S>(monitor_index, pace_to_fps, flags)?;
            Ok(CaptureHandle { backend: Backend::Wgc(session), sink })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that panics in a callback must not take the control loop with
    /// it. The panic already costs the frame; if it also poisoned the handle,
    /// `record.rs` could never call `finish()` and the MP4 would be left
    /// without its `moov` atom — unplayable, from one bad frame.
    #[test]
    fn a_panicking_sink_does_not_lock_out_the_control_loop() {
        let sink = SinkRef::new(7u32);
        let poisoner = sink.clone();
        let panicked = std::thread::spawn(move || {
            let mut held = poisoner.lock();
            *held = 9;
            panic!("a frame callback went down holding the lock");
        })
        .join();
        assert!(panicked.is_err(), "the test's own panic must have happened");

        assert_eq!(*sink.lock(), 9, "the sink is still readable, with its last state");
    }
}
