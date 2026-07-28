//! The clip library's on-disk format.
//!
//! One `.mp4` and one `.json` sidecar sharing a stem, flat in one directory.
//! Not a database: a crash mid-write costs one clip's metadata instead of the
//! library, deleting an `.mp4` in Explorer leaves an orphan that the next scan
//! prunes rather than a phantom row, and SQLite would be a C dependency and a
//! ~1 MB binary bump against a 1.5 MB engine. See spec §5.2.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use trix_proto::ClipMeta;
use windows::Win32::System::{
    SystemInformation::GetLocalTime,
    SystemServices::TIME_ZONE_ID_DAYLIGHT,
    Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION},
};

pub fn mp4_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.mp4"))
}

pub fn sidecar_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// Thumbnails are written by the daemon starting in stage 3; the path shape
/// is fixed here so deletion already removes them.
pub fn thumb_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jpg"))
}

/// True if `id` is a syntactically valid clip id: `YYYYMMDD_HHMMSS`, with an
/// optional `_N` collision suffix.
///
/// Ids arrive from the control socket and are concatenated into filesystem
/// paths. Everything that turns an id into a path calls this first — a
/// whitelist of digits and two underscores cannot express `..`, a separator,
/// a drive letter, or a UNC prefix.
pub fn is_valid_id(id: &str) -> bool {
    let Some((date, rest)) = id.split_once('_') else { return false };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if date.len() != 8 || !all_digits(date) {
        return false;
    }
    match rest.split_once('_') {
        None => rest.len() == 6 && all_digits(rest),
        Some((time, suffix)) => time.len() == 6 && all_digits(time) && all_digits(suffix),
    }
}

/// Picks an unused id for a clip being saved now, creating `dir` if needed.
pub fn allocate_clip_id(dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create clip directory {}", dir.display()))?;
    let now = unsafe { GetLocalTime() };
    let stem = format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    );
    Ok(next_free_id(dir, &stem))
}

/// Two clips in the same second get `_2`, `_3`, … rather than one silently
/// overwriting the other.
fn next_free_id(dir: &Path, stem: &str) -> String {
    if !mp4_path(dir, stem).exists() {
        return stem.to_string();
    }
    for n in 2u32.. {
        let candidate = format!("{stem}_{n}");
        if !mp4_path(dir, &candidate).exists() {
            return candidate;
        }
    }
    unreachable!("u32 range is not exhaustible in one second")
}

/// Writes the sidecar to a temp file and renames it into place, so a crash
/// mid-write cannot leave half-parsed JSON beside a good MP4.
pub fn write_sidecar(dir: &Path, meta: &ClipMeta) -> Result<()> {
    let final_path = sidecar_path(dir, &meta.id);
    let temp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(meta).context("serializing clip metadata")?;
    std::fs::write(&temp_path, json)
        .with_context(|| format!("writing {}", temp_path.display()))?;
    // Windows rename fails if the destination exists; rewriting a sidecar
    // (rename, favorite) is a normal operation.
    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&temp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))
}

pub fn read_sidecar(path: &Path) -> Result<ClipMeta> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Every clip in `dir`, newest first.
///
/// An `.mp4` with no sidecar gets one synthesized from the filesystem, so a
/// file dropped in by hand still shows up. A `.json` with no `.mp4` is an
/// orphan and is skipped. A malformed sidecar is warned about and its clip is
/// adopted as if the sidecar were missing — one bad file never hides a clip.
pub fn scan(dir: &Path) -> Result<Vec<ClipMeta>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No directory yet simply means no clips yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };

    let mut clips = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !is_valid_id(id) {
            continue;
        }
        let sidecar = sidecar_path(dir, id);
        let meta = match read_sidecar(&sidecar) {
            Ok(meta) => meta,
            Err(e) if sidecar.exists() => {
                tracing::warn!(clip = id, error = %e, "unreadable sidecar, adopting the clip");
                adopt(dir, id)
            }
            Err(_) => adopt(dir, id),
        };
        clips.push(meta);
    }
    // Ids are zero-padded timestamps, so lexicographic order is chronological.
    clips.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(clips)
}

/// Best-effort metadata for an `.mp4` with no usable sidecar. The fields that
/// need a demuxer to recover are left at zero rather than guessed.
fn adopt(dir: &Path, id: &str) -> ClipMeta {
    let bytes = std::fs::metadata(mp4_path(dir, id)).map(|m| m.len()).unwrap_or(0);
    ClipMeta {
        id: id.to_string(),
        title: format!("clip_{id}"),
        created: created_from_id(id),
        duration_ms: 0,
        bytes,
        width: 0,
        height: 0,
        fps: 0,
        encoder: String::new(),
        has_audio: false,
        favorite: false,
    }
}

/// `20260726_143012` -> `2026-07-26T14:30:12` with no offset claimed, since
/// an adopted file's original time zone is unknowable.
fn created_from_id(id: &str) -> String {
    let d = &id[..8];
    let t = &id[9..15];
    format!("{}-{}-{}T{}:{}:{}", &d[..4], &d[4..6], &d[6..8], &t[..2], &t[2..4], &t[4..6])
}

/// Local-time RFC 3339 stamp for a clip being saved now.
pub fn now_rfc3339_local() -> String {
    let now = unsafe { GetLocalTime() };
    let mut tz = TIME_ZONE_INFORMATION::default();
    // Windows defines UTC = local + Bias, so the ISO offset is the negation,
    // and the active seasonal bias has to be folded in or half the year is
    // reported an hour out.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias
        + if id == TIME_ZONE_ID_DAYLIGHT { tz.DaylightBias } else { tz.StandardBias };
    format_rfc3339(
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, -bias,
    )
}

/// Split out from [`now_rfc3339_local`] so the offset arithmetic is testable
/// without running in a particular time zone.
pub fn format_rfc3339(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    offset_minutes: i32,
) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.unsigned_abs();
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{sign}{:02}:{:02}",
        abs / 60,
        abs % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Istanbul is UTC+3; Windows reports that as a Bias of -180 minutes.
    #[test]
    fn rfc3339_renders_a_positive_offset() {
        assert_eq!(
            format_rfc3339(2026, 7, 26, 14, 30, 12, 180),
            "2026-07-26T14:30:12+03:00"
        );
    }

    #[test]
    fn rfc3339_renders_a_negative_and_a_zero_offset() {
        assert_eq!(format_rfc3339(2026, 1, 2, 3, 4, 5, -300), "2026-01-02T03:04:05-05:00");
        assert_eq!(format_rfc3339(2026, 1, 2, 3, 4, 5, 0), "2026-01-02T03:04:05+00:00");
        assert_eq!(
            format_rfc3339(2026, 1, 2, 3, 4, 5, 330),
            "2026-01-02T03:04:05+05:30",
            "half-hour zones are real (India, Newfoundland) and must not truncate"
        );
    }

    /// Clip ids arrive over the control socket and get concatenated into
    /// paths. This is the check that keeps them inside the clip directory.
    #[test]
    fn id_validation_rejects_anything_that_could_escape_the_clip_dir() {
        assert!(is_valid_id("20260726_143012"));
        assert!(is_valid_id("20260726_143012_2"));

        for bad in [
            "",
            "..",
            "../../Windows/System32/config/SAM",
            r"..\..\secrets",
            "20260726_143012/../x",
            r"C:\Windows\System32",
            r"\\server\share\x",
            "20260726",
            "2026072_143012",
            "20260726_14301",
            "20260726_143012_",
            "20260726_143012_a",
            "20260726_1430l2",
            "20260726_143012.mp4",
        ] {
            assert!(!is_valid_id(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn ids_do_not_collide_within_one_second() {
        let dir = std::env::temp_dir().join(format!("trix-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("20260726_143012.mp4"));

        std::fs::write(dir.join("20260726_143012.mp4"), b"x").unwrap();
        let next = next_free_id(&dir, "20260726_143012");
        assert_eq!(next, "20260726_143012_2");

        std::fs::write(dir.join("20260726_143012_2.mp4"), b"x").unwrap();
        assert_eq!(next_free_id(&dir, "20260726_143012"), "20260726_143012_3");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_skips_orphaned_sidecars_and_adopts_bare_mp4s() {
        let dir = std::env::temp_dir().join(format!("trix-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A complete clip.
        std::fs::write(dir.join("20260726_100000.mp4"), b"video").unwrap();
        write_sidecar(&dir, &sample_meta("20260726_100000")).unwrap();
        // A sidecar whose mp4 was deleted in Explorer: pruned, not listed.
        write_sidecar(&dir, &sample_meta("20260726_090000")).unwrap();
        // An mp4 with no sidecar: adopted so hand-dropped files still appear.
        std::fs::write(dir.join("20260726_110000.mp4"), b"video").unwrap();

        let clips = scan(&dir).unwrap();
        let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["20260726_110000", "20260726_100000"], "newest first");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn sample_meta(id: &str) -> trix_proto::ClipMeta {
        trix_proto::ClipMeta {
            id: id.to_string(),
            title: format!("clip_{id}"),
            created: "2026-07-26T10:00:00+03:00".into(),
            duration_ms: 15_000,
            bytes: 5,
            width: 1920,
            height: 1080,
            fps: 60,
            encoder: "test".into(),
            has_audio: true,
            favorite: false,
        }
    }
}
