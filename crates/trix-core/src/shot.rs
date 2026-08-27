//! Screenshots on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use trix_proto::ShotMeta;

use crate::library;

/// The subfolder screenshots live in, under the configured clips folder.
///
/// A subfolder rather than the clips folder itself, because `library::thumb_path`
/// is already `{clip_dir}\{id}.jpg` — a screenshot written beside the clips
/// under the same id grammar would silently overwrite a clip's thumbnail. This
/// removes the collision structurally instead of by picking a different suffix
/// and hoping nobody picks it again.
pub fn shots_dir(clip_dir: &Path) -> PathBuf {
    clip_dir.join("Screenshots")
}

/// The screenshot itself, at full capture resolution.
pub fn image_path(shots: &Path, id: &str) -> PathBuf {
    shots.join(format!("{id}.jpg"))
}

/// The 640 px copy the grid draws.
///
/// `{id}.thumb.jpg` is chosen so its file stem (`{id}.thumb`) fails
/// `library::is_valid_id` — a dot is outside that whitelist. That is what stops
/// [`scan`] listing thumbnails as screenshots in their own right, and a test
/// holds it rather than this comment.
pub fn thumb_path(shots: &Path, id: &str) -> PathBuf {
    shots.join(format!("{id}.thumb.jpg"))
}

/// Picks an unused id for a screenshot being saved now, creating `shots` if
/// needed, and **reserves it by creating the `.jpg` empty**.
///
/// The reservation is the contract `library::allocate_clip_id` documents at
/// length: ids are wall-clock stamps, so two savers in the same second want the
/// same name, and asking whether a file exists before creating it is a
/// check-then-act that loses one of them. `create_new` is atomic against every
/// other thread and every other process.
///
/// The caller owns the file from here on and must either overwrite it or delete
/// it. [`scan`] skips zero-byte entries, so a reservation stranded by a dying
/// process is invisible rather than a permanently broken tile.
pub fn allocate_shot_id(shots: &Path) -> Result<String> {
    std::fs::create_dir_all(shots)
        .with_context(|| format!("could not create screenshot folder {}", shots.display()))?;
    let stamp = library::now_id_stamp();
    for suffix in 1u32..=10_000 {
        let id = if suffix == 1 { stamp.clone() } else { format!("{stamp}_{suffix}") };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(image_path(shots, &id))
        {
            Ok(_) => return Ok(id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // Any other error stops the loop rather than advancing the suffix.
            // A folder that cannot be written to would otherwise be mistaken
            // for one that is full, and the caller would wait out ten thousand
            // failing opens to be told the wrong thing.
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("reserving {id} in {}", shots.display()));
            }
        }
    }
    bail!("could not find a free screenshot id in {} after 10000 tries", shots.display())
}

/// Every screenshot in `shots`, newest first.
///
/// A missing folder is an empty library, not an error: it is created by the
/// first screenshot, so every install is in that state until then and the tab
/// still has to open.
pub fn scan(shots: &Path) -> Result<Vec<ShotMeta>> {
    let entries = match std::fs::read_dir(shots) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", shots.display())),
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jpg") {
            continue;
        }
        // `{id}.thumb.jpg` has the stem `{id}.thumb`, which `is_valid_id`
        // rejects. This single check keeps thumbnails out of the grid.
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !library::is_valid_id(id) {
            continue;
        }
        // Taken off the directory entry rather than with a fresh `metadata`
        // call: on Windows the size came back with the enumeration.
        //
        // A metadata *error* is treated as "not zero", exactly as
        // `library::scan` does for clips and for the reason its comment gives:
        // the file is there, something is wrong with reading it, and hiding a
        // screenshot is the worse of the two mistakes. Folding an error into
        // zero would make a transient antivirus lock delete a tile from the
        // grid with nothing said anywhere.
        let metadata = entry.metadata();
        if metadata.as_ref().is_ok_and(|m| m.len() == 0) {
            // A reservation whose writer died, not a screenshot.
            continue;
        }
        let bytes = metadata.map(|m| m.len()).unwrap_or(0);
        let (width, height) = read_dimensions(&path).unwrap_or((0, 0));
        found.push(ShotMeta {
            created: library::created_from_id(id),
            id: id.to_string(),
            bytes,
            width,
            height,
        });
    }
    // Ids are zero-padded wall-clock stamps, so lexical order is chronological.
    found.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(found)
}

/// Removes a screenshot and its thumbnail.
///
/// The `.jpg` is the screenshot: if it is not there, there is nothing to delete
/// and that is an error. The thumbnail is a derived file, so its absence is not
/// — a screenshot whose thumbnail encode failed is still perfectly deletable.
pub fn delete(shots: &Path, id: &str) -> Result<()> {
    // Before a single path is built, not after. This is the seal that stops a
    // hostile id from the control socket reaching the filesystem at all.
    if !library::is_valid_id(id) {
        bail!("{id:?} is not a screenshot id");
    }
    std::fs::remove_file(image_path(shots, id))
        .with_context(|| format!("could not delete screenshot {id}"))?;
    match std::fs::remove_file(thumb_path(shots, id)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(shot = %id, error = %e, "screenshot thumbnail not removed"),
    }
    Ok(())
}

/// Reads only as much of a JPEG as the `SOF` marker needs.
fn read_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    // Enough for the SOI, an APP0/JFIF block, an EXIF block and the SOF after
    // them. Reading whole files instead would make opening the tab cost the
    // entire library in I/O; a JPEG whose SOF sits past this reports 0x0, and
    // the grid draws from the image itself anyway.
    let mut head = vec![0u8; 4096];
    let read = file.read(&mut head).ok()?;
    head.truncate(read);
    jpeg_dimensions(&head)
}

/// Width and height from a JPEG's `SOF` marker, or `None`.
///
/// Every access is length-checked. This parses bytes that came off disk in a
/// `panic = "abort"` build, where an index past the end of a truncated file is
/// not an error to recover from.
pub fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    loop {
        // Markers are 0xFF followed by a type; runs of 0xFF are legal padding.
        if i + 1 >= bytes.len() {
            return None;
        }
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        // Skip the fill bytes: a marker may be preceded by any number of extra
        // 0xFFs, and reading one of them as the marker type sends the parse
        // off into the middle of a segment.
        let mut type_at = i + 1;
        while type_at < bytes.len() && bytes[type_at] == 0xFF {
            type_at += 1;
        }
        if type_at >= bytes.len() {
            return None;
        }
        let marker = bytes[type_at];
        i = type_at + 1;
        // Standalone markers carry no length payload.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 1 >= bytes.len() {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[i], bytes[i + 1]]));
        // SOF0..SOF15, minus the DHT (0xC4), JPG (0xC8) and DAC (0xCC) markers
        // that share the range without being frame headers.
        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC;
        if is_sof {
            // length(2) + precision(1) + height(2) + width(2)
            if i + 6 >= bytes.len() {
                return None;
            }
            let height = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]);
            let width = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
            return Some((u32::from(width), u32::from(height)));
        }
        if length < 2 {
            return None;
        }
        i += length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but real JPEG: SOI, then an SOF0 segment. The marker stores
    /// **height before width**, which is the easiest thing in the format to
    /// get backwards, so the fixture is deliberately non-square.
    fn sof0(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x22, 0x00]);
        bytes
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("trix-shot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn dimensions_come_off_the_sof_marker_width_first() {
        assert_eq!(jpeg_dimensions(&sof0(1920, 1200)), Some((1920, 1200)));
    }

    #[test]
    fn a_truncated_jpeg_yields_no_dimensions_rather_than_panicking() {
        let full = sof0(1920, 1200);
        // The whole point: this build is `panic = "abort"`, so an index past
        // the end of a truncated file is not an error to recover from -- it is
        // the daemon dying with a live capture ring in hand.
        for cut in 0..full.len() {
            let _ = jpeg_dimensions(&full[..cut]);
        }
        assert_eq!(jpeg_dimensions(&full[..6]), None);
    }

    #[test]
    fn a_file_that_is_not_a_jpeg_yields_no_dimensions() {
        assert_eq!(jpeg_dimensions(b"not a jpeg at all"), None);
        assert_eq!(jpeg_dimensions(&[]), None);
    }

    #[test]
    fn fill_bytes_before_a_marker_are_skipped() {
        // A marker may be preceded by any number of extra 0xFFs. Reading one
        // of them as the marker type walks the parse into the middle of a
        // segment and yields either nothing or a wrong answer -- the loop's
        // own comment claimed this was handled before it was.
        let plain = sof0(1920, 1200);
        let mut padded = vec![0xFF, 0xD8];
        padded.push(0xFF);
        padded.extend_from_slice(&plain[2..]);
        assert_eq!(jpeg_dimensions(&padded), Some((1920, 1200)));
    }

    #[test]
    fn the_shots_folder_is_a_subfolder_of_the_clips_folder() {
        assert_eq!(
            shots_dir(Path::new(r"D:\Videos\Trix")),
            PathBuf::from(r"D:\Videos\Trix\Screenshots")
        );
    }

    #[test]
    fn a_thumbnail_stem_is_not_a_valid_id_so_scanning_cannot_list_it() {
        // This one fact is the entire mechanism keeping thumbnails out of the
        // grid: `{id}.thumb.jpg` has the stem `{id}.thumb`, and a dot is
        // outside `is_valid_id`'s whitelist. If that stops being true, every
        // screenshot appears twice.
        let thumb = thumb_path(Path::new(r"D:\shots"), "20260827_143012");
        let stem = thumb.file_stem().and_then(|s| s.to_str()).expect("stem");
        assert_eq!(stem, "20260827_143012.thumb");
        assert!(!library::is_valid_id(stem));
    }

    #[test]
    fn allocating_an_id_twice_in_one_second_suffixes_rather_than_colliding() {
        let dir = scratch("allocate");
        let first = allocate_shot_id(&dir).expect("first id");
        let second = allocate_shot_id(&dir).expect("second id");
        assert_ne!(first, second, "two shots must never be handed the same id");
        assert!(second.ends_with("_2"), "expected a _2 suffix, got {second}");
        assert!(image_path(&dir, &first).exists(), "the id must be reserved on disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_lists_images_newest_first_and_never_lists_thumbnails() {
        let dir = scratch("scan");
        for id in ["20260827_143012", "20260827_150000"] {
            std::fs::write(image_path(&dir, id), sof0(1920, 1200)).expect("image");
            std::fs::write(thumb_path(&dir, id), sof0(640, 400)).expect("thumb");
        }
        let shots = scan(&dir).expect("scan");
        assert_eq!(
            shots.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["20260827_150000", "20260827_143012"],
            "newest first, and exactly two -- the thumbnails are not shots"
        );
        assert_eq!((shots[0].width, shots[0].height), (1920, 1200));
        assert_eq!(shots[0].created, "2026-08-27T15:00:00");
        assert!(shots[0].bytes > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_a_folder_that_does_not_exist_is_empty_not_an_error() {
        // The folder is created by the first screenshot, so every install is
        // in this state until then. An error here would make the tab unopenable.
        let missing = std::env::temp_dir().join("trix-shot-never-created");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(scan(&missing).expect("empty, not an error").len(), 0);
    }

    #[test]
    fn scanning_skips_a_zero_byte_reservation() {
        // `allocate_shot_id` reserves by creating the file empty. A process
        // that died between reserving and writing must not leave a broken tile
        // in the grid forever.
        let dir = scratch("reservation");
        std::fs::write(image_path(&dir, "20260827_143012"), b"").expect("reservation");
        assert_eq!(scan(&dir).expect("scan").len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_removes_both_files_and_tolerates_a_missing_thumbnail() {
        let dir = scratch("delete");
        let id = "20260827_143012";

        std::fs::write(image_path(&dir, id), sof0(800, 600)).expect("image");
        delete(&dir, id).expect("delete with no thumbnail present");
        assert!(!image_path(&dir, id).exists());

        std::fs::write(image_path(&dir, id), sof0(800, 600)).expect("image");
        std::fs::write(thumb_path(&dir, id), sof0(640, 480)).expect("thumb");
        delete(&dir, id).expect("delete with both present");
        assert!(!image_path(&dir, id).exists());
        assert!(!thumb_path(&dir, id).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_hostile_id_touches_nothing() {
        // Ids arrive from the control socket. `is_valid_id` is a whitelist of
        // digits and underscores, which cannot express `..`, a separator, a
        // drive letter or a UNC prefix -- and it has to run before a path is
        // built, not after.
        let dir = scratch("hostile");
        for id in [r"..\..\Windows\System32\config", "a/b", "", "20260827_143012 "] {
            assert!(delete(&dir, id).is_err(), "{id:?} must be refused");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_clip_library_and_its_ceiling_never_see_the_screenshots_folder() {
        // The spec states screenshots are excluded from `max_library_gb`. That
        // is true structurally -- `library::scan` reads one directory without
        // recursing and keeps only `.mp4` -- and this is what keeps it true if
        // either side ever changes. Without it, the exclusion is a sentence in
        // a document rather than a property of the code.
        let clips = scratch("isolation");
        let shots = shots_dir(&clips);
        std::fs::create_dir_all(&shots).expect("shots dir");
        std::fs::write(image_path(&shots, "20260827_143012"), sof0(800, 600)).expect("shot");

        assert_eq!(library::scan(&clips).expect("clip scan").len(), 0);
        assert_eq!(library::prune_to_ceiling(&clips, 1).expect("prune").len(), 0);
        assert!(
            image_path(&shots, "20260827_143012").exists(),
            "the library ceiling must not touch screenshots"
        );
        let _ = std::fs::remove_dir_all(&clips);
    }
}
