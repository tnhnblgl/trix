pub mod audio;
pub mod video;

use std::time::Duration;

use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_SCHEDULINGPRIORITYCLASS_BELOW_NORMAL, D3DKMTSetProcessSchedulingPriorityClass,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows_capture::{
    graphics_capture_api::GraphicsCaptureApi,
    settings::{DrawBorderSettings, MinimumUpdateIntervalSettings},
};

/// Chooses the capture-border setting the running OS actually supports.
///
/// The WGC `GraphicsCaptureSession.IsBorderRequired` property — which
/// suppresses the yellow "you are being captured" border — only exists on
/// Windows 11 / Windows Server 2022 (build 20348) and newer. Consumer
/// Windows 10 (through build 19045 / 22H2) does not expose it, and
/// `windows-capture` rejects any non-`Default` border setting there with
/// `BorderConfigUnsupported`, aborting capture start entirely.
///
/// So query support first: hide the border where the platform allows it,
/// otherwise fall back to the OS default (border shown) so capture still
/// runs. This is an OS-build gate, not a GPU/vendor issue.
pub fn border_settings() -> DrawBorderSettings {
    match GraphicsCaptureApi::is_border_settings_supported() {
        Ok(true) => DrawBorderSettings::WithoutBorder,
        _ => DrawBorderSettings::Default,
    }
}

/// Asks WGC not to deliver frames faster than ~4/3 of the target fps, where
/// the OS supports it (`GraphicsCaptureSession.MinUpdateInterval`, Windows 11
/// 24H2+; same runtime-gate pattern as [`border_settings`]).
///
/// On high-refresh monitors this stops DWM from copying 144/165 frames per
/// second into the frame pool when the encoder only wants 60. The interval is
/// deliberately 3/4 of a frame period — a full period would beat against the
/// compositor's own cadence and halve the delivery rate; exact pacing to the
/// target fps is done by the sessions' QPC pacer instead.
pub fn min_update_interval(fps: u32) -> MinimumUpdateIntervalSettings {
    match GraphicsCaptureApi::is_minimum_update_interval_supported() {
        Ok(true) => {
            let interval = Duration::from_micros(u64::from(750_000 / fps.max(1)));
            tracing::info!(?interval, "capping WGC frame delivery near the target fps");
            MinimumUpdateIntervalSettings::Custom(interval)
        }
        _ => MinimumUpdateIntervalSettings::Default,
    }
}

/// Drops this process's GPU scheduling priority below normal so the game's
/// render work always preempts our BGRA→NV12 blits and encode submissions.
/// On a single-GPU machine that turns GPU contention into (rare) capture
/// frame drops instead of lost game fps. Failure is logged and ignored —
/// capture just runs at normal priority.
pub fn lower_gpu_priority() {
    let status = unsafe {
        D3DKMTSetProcessSchedulingPriorityClass(
            GetCurrentProcess(),
            D3DKMT_SCHEDULINGPRIORITYCLASS_BELOW_NORMAL,
        )
    };
    if status.is_ok() {
        tracing::info!("GPU scheduling priority lowered (game work preempts capture)");
    } else {
        tracing::warn!(?status, "could not lower GPU scheduling priority");
    }
}
