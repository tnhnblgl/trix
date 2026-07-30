//! `trix probe` — validates the riskiest platform assumptions before any
//! pipeline code exists: can we see the monitors (DXGI) and does the machine
//! expose hardware video encoder MFTs (Media Foundation)?
//!
//! The enumeration is also the daemon's answer to `monitors.list` and
//! `encoders.list`, so it is split from the printing: [`monitors`] and
//! [`encoders`] produce the data, [`print_monitors`] and [`print_encoders`] are
//! loops over it. `trix probe`'s stdout is a contract — same lines, same order,
//! same spacing — so the printers own every formatting decision and the data
//! types carry none of it (see [`CODEC_LABEL_WIDTH`]).

use anyhow::{Context, Result};
use serde::Serialize;
use windows::{
    Win32::{
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
        Media::MediaFoundation::{
            IMFActivate, MF_API_VERSION, MF_SDK_VERSION, MFMediaType_Video, MFSTARTUP_NOSOCKET,
            MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
            MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute,
            MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_AV1, MFVideoFormat_H264,
            MFVideoFormat_HEVC,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            SystemInformation::OSVERSIONINFOW,
        },
    },
    core::{GUID, PWSTR, s, w},
};

pub fn run() -> Result<()> {
    println!("trix probe — platform capability report");
    println!("=======================================");

    print_os_version();
    print_monitors().context("DXGI monitor enumeration failed")?;
    print_encoders().context("Media Foundation encoder enumeration failed")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// OS version
// ---------------------------------------------------------------------------

/// `RtlGetVersion` reports the true OS version regardless of manifest
/// compatibility shims (unlike `GetVersionExW`). It lives in ntdll, so we
/// resolve it dynamically instead of pulling in the WDK bindings.
fn print_os_version() {
    type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };

    let queried = unsafe {
        GetModuleHandleW(w!("ntdll.dll"))
            .ok()
            .and_then(|ntdll| GetProcAddress(ntdll, s!("RtlGetVersion")))
            .is_some_and(|proc| {
                let rtl_get_version: RtlGetVersionFn = std::mem::transmute(proc);
                rtl_get_version(&mut info) == 0
            })
    };

    println!("\n[OS]");
    if queried {
        let marketing = if info.dwBuildNumber >= 22000 { "Windows 11" } else { "Windows 10" };
        println!(
            "  {} (NT {}.{}, build {})",
            marketing, info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        );
        if info.dwBuildNumber < 18362 {
            println!("  WARNING: Windows.Graphics.Capture needs build 18362 (1903) or newer");
        }
    } else {
        println!("  could not query version (RtlGetVersion unavailable)");
    }
}

// ---------------------------------------------------------------------------
// Monitors / adapters (DXGI)
// ---------------------------------------------------------------------------

/// One desktop-attached monitor, in the same index order `monitor_index` uses.
///
/// [`Self::index`] is a position in [`monitors`]'s list, counting only
/// desktop-attached outputs across every adapter in DXGI's order — which is
/// exactly what `config.monitor_index` selects, and what `replay.rs` turns into
/// windows-capture's 1-based index by adding one. The two enumerations agreeing
/// is not obvious and is not assumed; `probe.rs`'s test suite asserts it.
#[derive(Debug, Clone, Serialize)]
pub struct MonitorInfo {
    pub index: u32,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
    pub adapter: String,
}

/// One DXGI adapter and the desktop-attached monitors hanging off it.
///
/// Private, and not the shape [`monitors`] returns: the report groups monitors
/// under their GPU because that is how it prints them, while a settings
/// dropdown wants one flat list in `monitor_index` order. Enumerating once into
/// this and projecting is what keeps the two from drifting — the alternative,
/// two passes over DXGI, is two chances for the printed list and the served
/// list to disagree about which screen is number 2.
struct AdapterReport {
    index: u32,
    description: String,
    software: bool,
    /// Bytes, as DXGI reports them. The report divides; nothing else needs to.
    dedicated_video_memory: u64,
    monitors: Vec<MonitorInfo>,
}

/// Every desktop-attached monitor, flat, in `config.monitor_index` order.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    Ok(adapters()?.into_iter().flat_map(|adapter| adapter.monitors).collect())
}

/// One pass over DXGI: every adapter, and the desktop-attached outputs of each.
fn adapters() -> Result<Vec<AdapterReport>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;

    let mut reports = Vec::new();
    let mut adapter_index = 0u32;
    // Deliberately counted across adapters rather than per-adapter: this is the
    // number a user picks in settings and `config.monitor_index` stores, so a
    // second GPU's first screen must not be "monitor 0" all over again.
    let mut monitor_index = 0u32;

    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let desc = unsafe { adapter.GetDesc1() }?;
        let description = wide_to_string(&desc.Description);

        let mut monitors = Vec::new();
        let mut output_index = 0u32;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            let out_desc = unsafe { output.GetDesc() }?;
            if out_desc.AttachedToDesktop.as_bool() {
                let r = out_desc.DesktopCoordinates;
                monitors.push(MonitorInfo {
                    index: monitor_index,
                    name: wide_to_string(&out_desc.DeviceName),
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                    left: r.left,
                    top: r.top,
                    adapter: description.clone(),
                });
                monitor_index += 1;
            }
            output_index += 1;
        }

        reports.push(AdapterReport {
            index: adapter_index,
            software: (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
            dedicated_video_memory: desc.DedicatedVideoMemory as u64,
            description,
            monitors,
        });
        adapter_index += 1;
    }

    Ok(reports)
}

/// Enumerated before the header is printed, matching what the single-pass
/// version did: a `CreateDXGIFactory1` that fails must not leave a `[GPUs &
/// monitors]` heading standing over nothing.
fn print_monitors() -> Result<()> {
    let adapters = adapters()?;

    println!("\n[GPUs & monitors]");
    let mut monitors_seen = 0usize;
    for adapter in &adapters {
        println!(
            "  GPU {}: {}{} ({} MB VRAM)",
            adapter.index,
            adapter.description,
            if adapter.software { " [software]" } else { "" },
            adapter.dedicated_video_memory / (1024 * 1024),
        );
        for monitor in &adapter.monitors {
            println!(
                "    Monitor {}: {} — {}x{} at ({}, {})",
                monitor.index, monitor.name, monitor.width, monitor.height, monitor.left,
                monitor.top,
            );
            monitors_seen += 1;
        }
    }

    if monitors_seen == 0 {
        println!("  WARNING: no desktop-attached monitors found");
    }
    Ok(())
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

// ---------------------------------------------------------------------------
// Hardware encoders (Media Foundation)
// ---------------------------------------------------------------------------

const MF_VERSION: u32 = (MF_SDK_VERSION << 16) | MF_API_VERSION;

/// One encoder MFT, as offered to the settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct EncoderInfo {
    /// `"H.264"`, `"HEVC"`, or `"AV1"` — unpadded. The report pads to
    /// [`CODEC_LABEL_WIDTH`] when it prints; a dropdown must not have to strip
    /// spaces the report needed for alignment.
    pub codec: String,
    pub name: String,
    /// False only for the software fallback, which is enumerated at all only
    /// when the machine offers no hardware encoder for any codec.
    pub hardware: bool,
}

/// The codecs the report covers, in the order it prints them.
const CODECS: [(&str, GUID); 3] =
    [("H.264", MFVideoFormat_H264), ("HEVC", MFVideoFormat_HEVC), ("AV1", MFVideoFormat_AV1)];

/// Column the report pads each codec label to, so the encoder names line up:
/// `"H.264"`, `"HEVC "`, `"AV1  "`. The widest label is `H.264` at 5. This
/// number is the whole reason [`EncoderInfo::codec`] can stay unpadded — the
/// padding used to be baked into the codec strings themselves.
const CODEC_LABEL_WIDTH: usize = 5;

/// Every encoder MFT the machine offers, hardware first.
///
/// The software fallback is enumerated only when *no* codec has a hardware
/// encoder — the same rule the report has always used, kept here rather than in
/// the printer so `encoders.list` offers a UI the same set the report describes.
pub fn encoders() -> Result<Vec<EncoderInfo>> {
    // COM outermost, Media Foundation inside it, each balanced on every path.
    // The `?` here is what makes the pairing correct rather than merely
    // symmetrical: `HRESULT::ok()` treats `S_OK` *and* `S_FALSE` as success,
    // and those are exactly the two returns that incremented this thread's
    // initialization count and therefore owe a `CoUninitialize`. The one
    // return that must **not** be paired — `RPC_E_CHANGED_MODE`, meaning the
    // thread is already in a single-threaded apartment and this call did
    // nothing — is a failure HRESULT, so it leaves through the `?` above the
    // uninitialize instead of through it.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("CoInitializeEx failed")?;

    // Whichever way the enumeration went. The single-pass version returned
    // early on an enumeration failure and left both of these unbalanced; that
    // never mattered for a one-shot `trix probe`, but `encoders.list` is
    // answered on a long-lived daemon that a UI can call as often as it likes,
    // and each unbalanced call would bump the calling session thread's COM
    // count for the life of that connection.
    let found = enumerate_with_media_foundation();
    unsafe { CoUninitialize() };
    found
}

/// [`encoders`]'s body between `MFStartup` and `MFShutdown`, split out so the
/// startup is paired by function scope rather than by a reader tracking two
/// early returns. The caller owns the COM half of the same pattern.
fn enumerate_with_media_foundation() -> Result<Vec<EncoderInfo>> {
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.context("MFStartup failed")?;

    let found = enumerate_encoders();
    let shutdown = unsafe { MFShutdown() }.context("MFShutdown failed");

    let found = found?;
    shutdown?;
    Ok(found)
}

fn enumerate_encoders() -> Result<Vec<EncoderInfo>> {
    let mut found = Vec::new();
    for (codec, subtype) in CODECS {
        for name in enum_encoders(subtype, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER)? {
            found.push(EncoderInfo { codec: codec.to_string(), name, hardware: true });
        }
    }

    // Everything pushed above is hardware, so an empty list here *is* "no
    // hardware encoder on this machine".
    if found.is_empty() {
        let software =
            enum_encoders(MFVideoFormat_H264, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER)?;
        for name in software {
            found.push(EncoderInfo { codec: "H.264".to_string(), name, hardware: false });
        }
    }
    Ok(found)
}

fn print_encoders() -> Result<()> {
    let found = encoders()?;

    println!("\n[Hardware video encoders (Media Foundation)]");
    for (codec, _) in CODECS {
        let mut listed = false;
        for encoder in found.iter().filter(|e| e.hardware && e.codec == codec) {
            println!("  {:<width$}: {}", encoder.codec, encoder.name, width = CODEC_LABEL_WIDTH);
            listed = true;
        }
        if !listed {
            println!("  {:<width$}: none", codec, width = CODEC_LABEL_WIDTH);
        }
    }

    if !found.iter().any(|e| e.hardware) {
        println!("\n  WARNING: no hardware encoders found; recording would use software");
        for encoder in found.iter().filter(|e| !e.hardware) {
            println!("  {} (software): {}", encoder.codec, encoder.name);
        }
    }

    Ok(())
}

/// Returns the friendly names of encoder MFTs producing `subtype` video.
fn enum_encoders(
    subtype: GUID,
    flags: windows::Win32::Media::MediaFoundation::MFT_ENUM_FLAG,
) -> Result<Vec<String>> {
    let output_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };

    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(&output_type),
            &mut activates,
            &mut count,
        )
    }
    .context("MFTEnumEx failed")?;

    let mut names = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // Take ownership so the interface is released when dropped.
        let activate = unsafe { std::ptr::read(activates.add(i)) };
        if let Some(activate) = activate {
            names.push(friendly_name(&activate));
        }
    }
    if !activates.is_null() {
        unsafe { CoTaskMemFree(Some(activates as *const _)) };
    }
    Ok(names)
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut value = PWSTR::null();
    let mut length = 0u32;
    let result =
        unsafe { activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut value, &mut length) };
    match result {
        Ok(()) if !value.is_null() => {
            let name = unsafe { value.to_string() }.unwrap_or_else(|_| "<invalid utf-16>".into());
            unsafe { CoTaskMemFree(Some(value.as_ptr() as *const _)) };
            name
        }
        _ => "<unnamed encoder>".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_capture::monitor::Monitor;

    /// `config.monitor_index` is a *position in this list*, so the list has to
    /// be positions: zero-based, dense, no gaps. A settings dropdown binds
    /// `MonitorInfo::index` straight to that config key.
    #[test]
    fn monitors_are_indexed_from_zero_without_gaps() {
        let monitors = monitors().expect("DXGI monitor enumeration must succeed");
        assert!(!monitors.is_empty(), "a machine running this test has a desktop attached");

        for (position, monitor) in monitors.iter().enumerate() {
            assert_eq!(
                monitor.index as usize, position,
                "monitor indices are positions in this list, not DXGI output ordinals"
            );
            assert!(monitor.width > 0 && monitor.height > 0, "monitor {position} has no size");
            assert!(!monitor.name.is_empty(), "monitor {position} has no device name");
            assert!(!monitor.adapter.is_empty(), "monitor {position} names no adapter");
        }
    }

    /// The index space `monitors()` publishes and the one the engine actually
    /// captures from have to be the same one, or the settings dropdown silently
    /// points at the wrong screen. `replay.rs`'s `start_session` derives the
    /// engine's via `Monitor::from_index(config.monitor_index as usize + 1)` —
    /// windows-capture counts from 1, `config.monitor_index` from 0 — and that
    /// exact expression is what this reproduces. The two enumerations are
    /// genuinely different walks: DXGI goes adapter by adapter and output by
    /// output, while windows-capture's `Monitor::enumerate` is
    /// `EnumDisplayMonitors` order. Nothing guarantees they agree; this is what
    /// checks it.
    ///
    /// **Identity is compared by device name, not by size, and that is not the
    /// obvious choice — it is the correct one.** The two APIs report a
    /// different *unit*: DXGI's `DesktopCoordinates` is virtualized for a
    /// DPI-unaware process (this one), so a 1920x1200 panel at 125% scaling
    /// reads 1536x960, while `Monitor::width`/`height` call
    /// `EnumDisplaySettingsW`, which is never virtualized and reports the real
    /// 1920x1200. Asserting equal sizes therefore fails on every scaled display
    /// while proving nothing about ordering. `\\.\DISPLAY1` is the same string
    /// in both APIs, is what actually names a screen, and does not move when
    /// someone changes their scaling. The size relationship is still checked,
    /// as the uniform ratio it is.
    ///
    /// Two claims live here and they need different amounts of hardware. The
    /// **offset** (`config.monitor_index` 0 is windows-capture's 1) is
    /// checkable with a single screen and is checked on every machine. The
    /// **ordering** (which physical screen is number 2) is only falsifiable
    /// with two or more attached — with one screen every enumeration order is
    /// the same order — so on a single-monitor machine that half is reported as
    /// unproven rather than silently passing. Run the suite with
    /// `cargo test -- --nocapture` to see the note.
    #[test]
    fn dxgi_and_windows_capture_agree_on_the_monitor_index_space() {
        let monitors = monitors().expect("DXGI monitor enumeration must succeed");

        if monitors.is_empty() {
            eprintln!(
                "SKIPPED dxgi_and_windows_capture_agree_on_the_monitor_index_space: \
                 no desktop-attached monitor to compare"
            );
            return;
        }
        if monitors.len() < 2 {
            eprintln!(
                "PARTIAL dxgi_and_windows_capture_agree_on_the_monitor_index_space: \
                 1 monitor attached — the +1 offset and the DXGI/windows-capture identity of \
                 monitor 0 are checked below, but monitor *ordering* agreement needs two or more \
                 screens and is left unproven on this machine"
            );
        }

        for info in &monitors {
            let captured = Monitor::from_index(info.index as usize + 1).unwrap_or_else(|e| {
                panic!(
                    "config.monitor_index {} is windows-capture index {}, which it does not have: \
                     {e}",
                    info.index,
                    info.index + 1
                )
            });

            let device_name = captured.device_name().expect("windows-capture device name");
            assert_eq!(
                device_name, info.name,
                "config.monitor_index {} is {:?} to DXGI but {device_name:?} to windows-capture — \
                 the settings dropdown and the capture session disagree about which screen that is",
                info.index, info.name,
            );

            // Same panel, different unit. Cross-multiplied so this is a
            // comparison of shapes rather than of pixel counts, with a
            // tolerance because DPI virtualization rounds: 1920x1200 at 175%
            // reads 1097x617, whose ratio is a hair off 16:10.
            let width = captured.width().expect("windows-capture monitor width") as f64;
            let height = captured.height().expect("windows-capture monitor height") as f64;
            let physical = width / height;
            let logical = f64::from(info.width) / f64::from(info.height);
            assert!(
                (physical - logical).abs() < logical * 0.01,
                "config.monitor_index {} is {}x{} to DXGI and {width}x{height} to \
                 windows-capture — a uniform DPI scale would keep the aspect ratio, so these are \
                 not the same display",
                info.index,
                info.width,
                info.height,
            );
        }
    }

    /// The split's own risk: the report used to carry its column padding inside
    /// the codec strings (`"HEVC "`, `"AV1  "`), and a UI given those would
    /// render a dropdown of trailing spaces. The data is unpadded; only
    /// [`print_encoders`] pads.
    #[test]
    fn encoders_are_labelled_by_unpadded_codec() {
        let encoders = encoders().expect("Media Foundation encoder enumeration must succeed");

        for encoder in &encoders {
            assert_eq!(encoder.codec.trim(), encoder.codec, "codec labels carry no padding");
            assert!(
                CODECS.iter().any(|(codec, _)| *codec == encoder.codec),
                "unexpected codec label {:?}",
                encoder.codec
            );
            assert!(!encoder.name.is_empty(), "an encoder with no name is not selectable");
        }

        // The fallback is all-or-nothing: software entries exist only when the
        // machine offers no hardware encoder at all.
        if encoders.iter().any(|e| e.hardware) {
            assert!(
                encoders.iter().all(|e| e.hardware),
                "the software fallback must not be mixed in with hardware encoders"
            );
        }
    }

    /// The widest label has to fit, or the report's columns move.
    #[test]
    fn every_codec_label_fits_the_report_column() {
        for (codec, _) in CODECS {
            assert!(
                codec.len() <= CODEC_LABEL_WIDTH,
                "{codec:?} is wider than the report's {CODEC_LABEL_WIDTH}-column label"
            );
        }
    }
}
