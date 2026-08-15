//! Replacing the running program with a newer copy of itself.
//!
//! Windows will not let a running executable be deleted or overwritten, but it
//! will let one be *renamed*. That single fact is the whole mechanism:
//! `trix-ui.exe` renames itself to `trix-ui.exe.old` while executing from the
//! renamed file, and the new build is moved into the name it just vacated.
//!
//! Everything here is ordered so that the install is never in a state that
//! cannot be put back. Each vacated name is recorded, and any failure undoes
//! exactly those in reverse before returning. Nothing in this module may panic:
//! the release profile is `panic = "abort"`, so a panic between two renames is
//! not a crash the user retries, it is an install left half-moved.

use std::path::{Path, PathBuf};

/// The three files that are renamed rather than overwritten, in swap order.
/// `trix-ui.exe` is last because it is the one doing the swapping -- if
/// anything is going to fail, it should fail before the running program has
/// moved itself.
pub const BINARIES: [&str; 3] = ["trix.exe", "trix-daemon.exe", "trix-ui.exe"];

/// Shipped alongside, never locked, so these are simply overwritten.
pub const DOCS: [&str; 2] = ["LICENSE", "README.txt"];

/// Staging lives inside the install directory, not in `%TEMP%`.
///
/// A rename is only cheap and near-atomic *within a volume*. A portable Trix on
/// `D:\` with `%TEMP%` on `C:\` would turn every move below into a copy, with a
/// correspondingly wider window in which to fail halfway.
pub const STAGING: &str = ".trix-update";

/// Suffix for the outgoing build, deleted on the next launch.
pub const OLD_SUFFIX: &str = ".old";

/// The folder Trix is installed in — where this executable lives.
pub fn install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not locate trix-ui.exe: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "could not work out which folder Trix is installed in".to_string())
}

/// Whether files can be created here, established by creating one.
///
/// Asked before anything is downloaded rather than discovered mid-swap. A
/// portable app dropped into `C:\Program Files` is not writable without
/// elevation, and finding that out after two of three binaries have moved is
/// how an install gets destroyed.
pub fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".trix-write-probe");
    std::fs::write(&probe, b"trix").map_err(|_| {
        format!(
            "Trix cannot update itself in {}, because it does not have permission to write there. \
             Move the Trix folder somewhere like your Documents, or download the update by hand.",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Extracts `zip` under `into` and returns the directory holding the binaries.
pub fn unpack(zip: &Path, into: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("could not open the update: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("the update is not a valid zip: {e}"))?;
    // `extract` validates each entry's CRC-32 as it writes, so a corrupt member
    // is caught here rather than by whatever tries to run it. It also refuses
    // entry names that escape `into` -- which is why this is the crate's own
    // `extract` on a current major rather than a hand-rolled write loop.
    archive.extract(into).map_err(|e| format!("could not unpack the update: {e}"))?;
    payload_root(into)
}

/// Finds the directory that actually holds the binaries.
///
/// `ship-zip.ps1` packs with `includeBaseDirectory: true`, so the archive holds
/// exactly one top-level folder (`trix-v0.5.0-win-x64`) rather than a flat set
/// of files. This resolves it by looking for the binaries instead of by
/// rebuilding the expected name, so a rename of the zip's inner folder does not
/// break the updater.
fn payload_root(extracted: &Path) -> Result<PathBuf, String> {
    let complete = |dir: &Path| BINARIES.iter().all(|name| dir.join(name).is_file());
    if complete(extracted) {
        return Ok(extracted.to_path_buf());
    }
    let entries = std::fs::read_dir(extracted)
        .map_err(|e| format!("could not read the unpacked update: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && complete(&path) {
            return Ok(path);
        }
    }
    Err("the downloaded update does not contain the Trix programs".to_string())
}

/// The one filesystem operation the swap turns on.
///
/// Injected rather than called directly because almost everything worth testing
/// in this module is downstream of a rename that failed -- a locked file, a
/// permission revoked between two moves, a disk that filled -- and none of that
/// can be provoked from a test against a real directory. Same reasoning as
/// `daemon.rs`'s `wait_until_gone(budget, poll, gone)`: the awkward half is
/// passed in so the interesting half can be exercised.
type Rename = dyn Fn(&Path, &Path) -> std::io::Result<()>;

/// Moves the payload into place, or puts everything back.
pub fn swap_in(install: &Path, payload: &Path) -> Result<(), String> {
    swap_with(install, payload, &|from, to| std::fs::rename(from, to))
}

fn swap_with(install: &Path, payload: &Path, rename: &Rename) -> Result<(), String> {
    // Checked before the first rename: a payload missing a file must fail while
    // the install is still untouched, not two-thirds of the way through. This
    // is a gate, not advice -- the cheapest good outcome available to this
    // module is a refusal that leaves the install exactly as it found it.
    for name in BINARIES {
        if !payload.join(name).is_file() {
            return Err(format!("the downloaded update is missing {name}, so nothing was changed"));
        }
    }

    // What actually happened, not what was planned: a name goes in here the
    // instant it is vacated, so rollback undoes the moves that really occurred
    // rather than assuming a fixed prefix of the loop completed.
    let mut vacated: Vec<&str> = Vec::new();
    for name in BINARIES {
        let live = install.join(name);
        let old = install.join(format!("{name}{OLD_SUFFIX}"));

        if let Err(e) = rename(&live, &old) {
            let cause = format!("could not move the old {name} aside: {e}");
            return Err(undo(install, &vacated, &cause, rename));
        }
        // Recorded between the two renames, not after them. Right here the
        // install has no `{name}` at all, and that gap is precisely the state
        // rollback exists to close.
        vacated.push(name);

        if let Err(e) = rename(&payload.join(name), &live) {
            let cause = format!("could not put the new {name} in place: {e}");
            return Err(undo(install, &vacated, &cause, rename));
        }
    }

    // Not renamed, so not rolled back: these are never locked, and a stale
    // README beside a correct set of binaries is cosmetic. Failing the whole
    // update over one would be worse than the inconsistency.
    for name in DOCS {
        let from = payload.join(name);
        if from.is_file() {
            let _ = std::fs::copy(&from, install.join(name));
        }
    }
    Ok(())
}

/// Puts the vacated names back and turns `cause` into what the user should do.
///
/// A failed rollback is a different event from a failed swap and says so: the
/// swap failing costs the user an update, while the rollback failing costs them
/// a working Trix, and only the second one asks anything of them. `trix-ui`
/// carries no logger (see `main.rs`), and in a release build there is no console
/// to print to anyway, so the distinction has to travel in the returned string —
/// the only channel this module has to the window that called it.
fn undo(install: &Path, vacated: &[&str], cause: &str, rename: &Rename) -> String {
    let stranded = roll_back(install, vacated, rename);
    if stranded.is_empty() {
        return format!(
            "{cause}. Trix was not updated; the version you were running is still installed."
        );
    }
    let names =
        stranded.iter().map(|name| format!("{name}{OLD_SUFFIX}")).collect::<Vec<_>>().join(", ");
    format!(
        "{cause}, and putting the previous version back failed too, so this Trix install is now \
         incomplete. In {}, rename {names} to drop the \"{OLD_SUFFIX}\" from each name — or \
         download Trix again from the releases page.",
        install.display()
    )
}

/// Restores the named binaries, and reports the ones it could not.
///
/// Returns the files still stranded under `.old`, which is the one outcome the
/// user has to act on. It cannot itself fail: there is nothing left to try.
fn roll_back<'a>(install: &Path, vacated: &[&'a str], rename: &Rename) -> Vec<&'a str> {
    let mut stranded = Vec::new();
    for name in vacated.iter().rev() {
        let live = install.join(name);
        let old = install.join(format!("{name}{OLD_SUFFIX}"));
        // One rename, not delete-then-rename. On Windows `fs::rename` is
        // `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, so this drops the old
        // file back over the new one in a single step. Deleting first would open
        // a window in which the name exists nowhere, and a rename that then
        // failed would leave the user with no file under that name at all --
        // turning a recoverable failure into an unrecoverable one.
        if rename(&old, &live).is_err() {
            stranded.push(*name);
        }
    }
    stranded
}

/// Deletes the previous build and the staging folder.
///
/// Called at startup, when nothing holds them open. Every failure is ignored on
/// purpose: a leftover `trix-ui.exe.old` is litter, not a fault, and the next
/// launch tries again. Reporting it would be an error message about a file the
/// user never knew existed.
pub fn cleanup(install: &Path) {
    for name in BINARIES {
        let _ = std::fs::remove_file(install.join(format!("{name}{OLD_SUFFIX}")));
    }
    let _ = std::fs::remove_dir_all(install.join(STAGING));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own. Never the real install: this
    /// module renames and deletes executables, and pointing it at a developer's
    /// own folder is the one mistake here that cannot be undone by rerunning.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trix-swap-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn install_with(dir: &Path, marker: &str) {
        for name in BINARIES.iter().chain(DOCS.iter()) {
            std::fs::write(dir.join(name), format!("{marker} {name}")).expect("write");
        }
    }

    fn contents(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn a_clean_swap_replaces_every_shipped_file_and_keeps_the_old_binaries() {
        let install = scratch("clean");
        install_with(&install, "old");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");

        swap_in(&install, &payload).expect("swap");

        for name in BINARIES {
            assert_eq!(contents(&install, name), format!("new {name}"), "{name} was not replaced");
            assert_eq!(
                contents(&install, &format!("{name}{OLD_SUFFIX}")),
                format!("old {name}"),
                "{name} must be recoverable until the new build has started"
            );
        }
        for name in DOCS {
            assert_eq!(contents(&install, name), format!("new {name}"));
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    /// Nothing else in the folder is touched. Users unzip Trix into folders
    /// that already contain their own things.
    #[test]
    fn files_trix_did_not_ship_are_left_alone() {
        let install = scratch("bystanders");
        install_with(&install, "old");
        std::fs::write(install.join("my-notes.txt"), "mine").expect("write");
        std::fs::create_dir_all(install.join("clips")).expect("dir");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");

        swap_in(&install, &payload).expect("swap");

        assert_eq!(contents(&install, "my-notes.txt"), "mine");
        assert!(install.join("clips").is_dir());
        let _ = std::fs::remove_dir_all(&install);
    }

    /// The failure that matters. A payload missing one binary must leave the
    /// install exactly as it was -- not two-thirds updated.
    #[test]
    fn an_incomplete_payload_rolls_every_file_back() {
        let install = scratch("rollback");
        install_with(&install, "old");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");
        // trix-ui.exe is swapped last, so a payload checked file-by-file as the
        // loop reached it would only notice this after the other two had been
        // replaced. `swap_in`'s preflight is what turns it into a refusal
        // instead of a rollback -- and a refusal is the better outcome, because
        // nothing was moved to put back.
        std::fs::remove_file(payload.join("trix-ui.exe")).expect("remove");

        let error = swap_in(&install, &payload).expect_err("an incomplete payload must fail");
        assert!(error.contains("trix-ui.exe"), "the message must name the file: {error}");

        for name in BINARIES {
            assert_eq!(
                contents(&install, name),
                format!("old {name}"),
                "{name} must be back exactly as it was"
            );
            assert!(
                !install.join(format!("{name}{OLD_SUFFIX}")).exists(),
                "a rolled-back swap must leave no .old files behind"
            );
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    #[test]
    fn cleanup_removes_the_old_binaries_and_the_staging_folder() {
        let install = scratch("cleanup");
        install_with(&install, "new");
        for name in BINARIES {
            std::fs::write(install.join(format!("{name}{OLD_SUFFIX}")), "old").expect("write");
        }
        std::fs::create_dir_all(install.join(STAGING).join("download")).expect("dir");
        std::fs::write(install.join("my-notes.txt"), "mine").expect("write");

        cleanup(&install);

        for name in BINARIES {
            assert!(!install.join(format!("{name}{OLD_SUFFIX}")).exists());
            assert_eq!(
                contents(&install, name),
                format!("new {name}"),
                "the live build must survive"
            );
        }
        assert!(!install.join(STAGING).exists());
        assert_eq!(contents(&install, "my-notes.txt"), "mine");
        let _ = std::fs::remove_dir_all(&install);
    }

    /// ship-zip.ps1 packs with includeBaseDirectory, so the archive holds one
    /// top-level folder. Assuming a flat layout would look for the binaries in
    /// the wrong place every single time.
    #[test]
    fn the_payload_is_the_single_directory_inside_the_extraction() {
        let extracted = scratch("payload");
        let inner = extracted.join("trix-v0.5.0-win-x64");
        std::fs::create_dir_all(&inner).expect("dir");
        install_with(&inner, "new");

        assert_eq!(payload_root(&extracted).expect("resolves"), inner);
        let _ = std::fs::remove_dir_all(&extracted);
    }

    #[test]
    fn a_payload_without_the_three_binaries_is_refused() {
        let extracted = scratch("bad-payload");
        let inner = extracted.join("something-else");
        std::fs::create_dir_all(&inner).expect("dir");
        std::fs::write(inner.join("readme.md"), "not trix").expect("write");

        assert!(payload_root(&extracted).is_err());
        let _ = std::fs::remove_dir_all(&extracted);
    }

    #[test]
    fn a_writable_directory_is_recognised_and_the_probe_leaves_nothing() {
        let dir = scratch("writable");
        writable(&dir).expect("a temp dir is writable");
        assert_eq!(std::fs::read_dir(&dir).expect("read").count(), 0, "the probe must clean up");
        assert!(writable(Path::new(r"C:\Windows\System32\__trix_nope__")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- the paths that only appear when something goes wrong ----------------
    //
    // The seven tests above cannot reach a rollback. `swap_in` refuses an
    // incomplete payload before it renames anything, which is the right
    // behaviour and also means the whole recovery path -- the part of this
    // module that exists to save an install -- is never executed by them. The
    // failures that do reach it are a rename refused by the filesystem: a
    // locked file, a revoked permission, a disk that filled between two moves.
    // None of those can be provoked reliably from a test, so the rename itself
    // is the seam, and these drive it.

    /// A rename that works, except from the sources named. Keyed on the source
    /// path rather than on a call count so each test says which *step* it is
    /// breaking, and stays honest if the order of the moves ever changes.
    fn rename_refusing(deny: Vec<PathBuf>) -> impl Fn(&Path, &Path) -> std::io::Result<()> {
        move |from, to| {
            if deny.iter().any(|denied| denied == from) {
                Err(std::io::Error::other("injected rename failure"))
            } else {
                std::fs::rename(from, to)
            }
        }
    }

    /// Sets up an install and a payload beside it, both fully populated.
    fn install_and_payload(name: &str) -> (PathBuf, PathBuf) {
        let install = scratch(name);
        install_with(&install, "old");
        let payload = install.join(STAGING).join("staged");
        std::fs::create_dir_all(&payload).expect("payload");
        install_with(&payload, "new");
        (install, payload)
    }

    fn assert_install_is_untouched(install: &Path) {
        for name in BINARIES {
            assert_eq!(
                contents(install, name),
                format!("old {name}"),
                "{name} must hold exactly what it held before the swap"
            );
            assert!(
                !install.join(format!("{name}{OLD_SUFFIX}")).exists(),
                "{name}{OLD_SUFFIX} must not survive a rolled-back swap"
            );
        }
    }

    /// The first move failing is the cheap case: nothing has been vacated, so
    /// there is nothing to undo.
    #[test]
    fn a_failure_on_the_very_first_move_leaves_the_install_untouched() {
        let (install, payload) = install_and_payload("first-move");
        let rename = rename_refusing(vec![install.join("trix.exe")]);

        let error = swap_with(&install, &payload, &rename).expect_err("the swap must fail");

        assert!(error.contains("trix.exe"), "the message must name the file: {error}");
        assert!(
            error.contains("still installed"),
            "a clean refusal must tell the user they lost nothing: {error}"
        );
        assert_install_is_untouched(&install);
        let _ = std::fs::remove_dir_all(&install);
    }

    /// The case the module exists for: one binary is already swapped when a
    /// later move fails, and it has to go back.
    #[test]
    fn a_failure_partway_through_puts_the_already_swapped_binaries_back() {
        let (install, payload) = install_and_payload("partway");
        // trix.exe swaps cleanly; trix-daemon.exe cannot be moved aside.
        let rename = rename_refusing(vec![install.join("trix-daemon.exe")]);

        let error = swap_with(&install, &payload, &rename).expect_err("the swap must fail");

        assert!(error.contains("trix-daemon.exe"), "the message must name the file: {error}");
        assert!(
            !payload.join("trix.exe").exists(),
            "the premise of this test: trix.exe really had been swapped in, so the assertion \
             below is about a file that was put back rather than one never touched"
        );
        assert_install_is_untouched(&install);
        let _ = std::fs::remove_dir_all(&install);
    }

    /// The narrowest window in the whole module: the old file has been renamed
    /// away and the new one has not arrived, so for an instant the name exists
    /// nowhere. A rollback that only handled *completed* pairs would walk past
    /// this and leave the user with no trix-ui.exe.
    #[test]
    fn a_name_vacated_but_not_yet_filled_is_still_put_back() {
        let (install, payload) = install_and_payload("vacated");
        let rename = rename_refusing(vec![payload.join("trix-ui.exe")]);

        let error = swap_with(&install, &payload, &rename).expect_err("the swap must fail");

        assert!(error.contains("trix-ui.exe"), "the message must name the file: {error}");
        assert_install_is_untouched(&install);
        let _ = std::fs::remove_dir_all(&install);
    }

    /// A failed rollback is not a failed swap. The swap failing costs an
    /// update; the rollback failing costs a working Trix, and only the second
    /// one asks the user to do something. The message has to be able to tell
    /// them apart, and to name the file left under `.old`.
    #[test]
    fn a_rollback_that_cannot_restore_a_file_says_the_install_is_incomplete() {
        let (install, payload) = install_and_payload("stranded");
        let rename = rename_refusing(vec![
            // Fails the swap of trix-ui.exe ...
            payload.join("trix-ui.exe"),
            // ... and then fails putting it back.
            install.join(format!("trix-ui.exe{OLD_SUFFIX}")),
        ]);

        let error = swap_with(&install, &payload, &rename).expect_err("the swap must fail");

        assert!(
            error.contains("incomplete"),
            "a stranded install must not read like an ordinary failed update: {error}"
        );
        assert!(
            error.contains(&format!("trix-ui.exe{OLD_SUFFIX}")),
            "the user has to be told which file to rename: {error}"
        );
        assert!(!install.join("trix-ui.exe").exists(), "the test's premise: the name is empty");
        assert!(
            install.join(format!("trix-ui.exe{OLD_SUFFIX}")).exists(),
            "the file the message names must actually be there"
        );
        // The two the rollback could reach are still put back, rather than
        // being abandoned because an earlier one failed.
        for name in ["trix.exe", "trix-daemon.exe"] {
            assert_eq!(contents(&install, name), format!("old {name}"));
            assert!(!install.join(format!("{name}{OLD_SUFFIX}")).exists());
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    /// LICENSE and README.txt are copied, not renamed, and are worth nothing
    /// next to a working binary. A payload without them must still update Trix.
    #[test]
    fn a_payload_missing_the_docs_still_swaps_the_binaries() {
        let (install, payload) = install_and_payload("no-docs");
        for name in DOCS {
            std::fs::remove_file(payload.join(name)).expect("remove");
        }

        swap_in(&install, &payload).expect("the docs are not worth failing an update over");

        for name in BINARIES {
            assert_eq!(contents(&install, name), format!("new {name}"));
        }
        for name in DOCS {
            assert_eq!(contents(&install, name), format!("old {name}"), "the old doc stays");
        }
        let _ = std::fs::remove_dir_all(&install);
    }

    /// Builds a zip the way `ship-zip.ps1` does: every entry under one folder.
    /// Stored rather than deflated, so what this exercises is the archive
    /// walking and the payload resolution, not miniz.
    fn write_zip(path: &Path, entries: &[(String, String)]) {
        let file = std::fs::File::create(path).expect("create zip");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(name.as_str(), options).expect("start entry");
            std::io::Write::write_all(&mut writer, body.as_bytes()).expect("write entry");
        }
        writer.finish().expect("finish zip");
    }

    /// End to end over the real crate: a zip on disk in, the directory holding
    /// the binaries out. `payload_root` is unit-tested above against a
    /// hand-made tree; this is the half that proves the tree it is given is the
    /// one an actual archive produces.
    #[test]
    fn unpack_extracts_an_archive_and_resolves_the_folder_inside_it() {
        let dir = scratch("unpack");
        let archive = dir.join("trix-v0.9.9-win-x64.zip");
        let entries: Vec<(String, String)> = BINARIES
            .iter()
            .chain(DOCS.iter())
            .map(|name| (format!("trix-v0.9.9-win-x64/{name}"), format!("new {name}")))
            .collect();
        write_zip(&archive, &entries);

        let into = dir.join(STAGING).join("staged");
        let payload = unpack(&archive, &into).expect("a well-formed archive must unpack");

        assert_eq!(payload, into.join("trix-v0.9.9-win-x64"), "the payload is the inner folder");
        for name in BINARIES.iter().chain(DOCS.iter()) {
            assert_eq!(contents(&payload, name), format!("new {name}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The checksum is verified before this runs, so reaching here with a
    /// non-archive means something is badly wrong -- it must still be an error
    /// rather than a panic, because a panic in this crate's release profile
    /// aborts the process.
    #[test]
    fn unpack_refuses_a_file_that_is_not_an_archive() {
        let dir = scratch("not-a-zip");
        let archive = dir.join("trix.zip");
        std::fs::write(&archive, b"this is not a zip file").expect("write");

        let error = unpack(&archive, &dir.join("staged")).expect_err("must refuse");

        assert!(error.contains("not a valid zip"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
