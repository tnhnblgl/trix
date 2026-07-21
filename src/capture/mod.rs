pub mod audio;
pub mod video;

use windows_capture::{graphics_capture_api::GraphicsCaptureApi, settings::DrawBorderSettings};

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
