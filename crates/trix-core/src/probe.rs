//! `trix probe` — validates the riskiest platform assumptions before any
//! pipeline code exists: can we see the monitors (DXGI) and does the machine
//! expose hardware video encoder MFTs (Media Foundation)?

use anyhow::{Context, Result};
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
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
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

fn print_monitors() -> Result<()> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;

    println!("\n[GPUs & monitors]");
    let mut adapter_index = 0u32;
    let mut monitor_index = 0u32;

    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let desc = unsafe { adapter.GetDesc1() }?;
        let software = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
        println!(
            "  GPU {}: {}{} ({} MB VRAM)",
            adapter_index,
            wide_to_string(&desc.Description),
            if software { " [software]" } else { "" },
            desc.DedicatedVideoMemory / (1024 * 1024),
        );

        let mut output_index = 0u32;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            let out_desc = unsafe { output.GetDesc() }?;
            if out_desc.AttachedToDesktop.as_bool() {
                let r = out_desc.DesktopCoordinates;
                println!(
                    "    Monitor {}: {} — {}x{} at ({}, {})",
                    monitor_index,
                    wide_to_string(&out_desc.DeviceName),
                    r.right - r.left,
                    r.bottom - r.top,
                    r.left,
                    r.top,
                );
                monitor_index += 1;
            }
            output_index += 1;
        }
        adapter_index += 1;
    }

    if monitor_index == 0 {
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

fn print_encoders() -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("CoInitializeEx failed")?;
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.context("MFStartup failed")?;

    println!("\n[Hardware video encoders (Media Foundation)]");
    let codecs = [
        ("H.264", MFVideoFormat_H264),
        ("HEVC ", MFVideoFormat_HEVC),
        ("AV1  ", MFVideoFormat_AV1),
    ];

    let mut hardware_found = false;
    for (name, subtype) in codecs {
        let encoders = enum_encoders(subtype, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER)?;
        if encoders.is_empty() {
            println!("  {}: none", name);
        } else {
            hardware_found = true;
            for encoder in encoders {
                println!("  {}: {}", name, encoder);
            }
        }
    }

    if !hardware_found {
        let software = enum_encoders(
            MFVideoFormat_H264,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        )?;
        println!("\n  WARNING: no hardware encoders found; recording would use software");
        for encoder in software {
            println!("  H.264 (software): {}", encoder);
        }
    }

    unsafe { MFShutdown() }.context("MFShutdown failed")?;
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
