//! The one place that decides whether a downloaded file may be installed.
//!
//! Today that decision is a SHA-256 published beside the asset, which catches
//! a corrupt or truncated download and a release assembled wrong. It does not
//! catch a hostile release: whoever can publish the zip can publish the digest.
//! The design accepts that (the GitHub account is the trust anchor) and keeps
//! this module separate so a signature check can be added here, alone, without
//! touching the download or the swap.

use std::path::Path;

use sha2::{Digest as _, Sha256};

/// The digest `SHA256SUMS.txt` publishes for `name`.
///
/// Lines must be exactly `<64 hex>  <filename>` (two spaces), which is what
/// `Get-FileHash` piped through `ship-zip.ps1` produces and what `sha256sum`
/// reads. Any other format is treated as "no checksum published" and the
/// install is blocked—this module must enforce the format, not infer it.
pub fn digest_for(sums: &str, name: &str) -> Result<String, String> {
    sums.lines()
        .filter_map(|line| line.split_once("  "))
        .find(|(_, file)| file.trim() == name)
        .map(|(digest, _)| digest.trim().to_ascii_lowercase())
        .ok_or_else(|| format!("the release does not publish a checksum for {name}"))
}

/// Streams the file rather than reading it into memory. The zip is a few
/// megabytes today, but a hashing function that must not be the reason a
/// machine with little RAM fails is worth writing once.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("could not read the downloaded file: {e}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("could not read the downloaded file: {e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// `Ok` only when `path` is exactly what the release says `name` should be.
pub fn check(path: &Path, sums: &str, name: &str) -> Result<(), String> {
    let expected = digest_for(sums, name)?;
    let actual = sha256_file(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name} did not match the checksum GitHub published for it, so it was not installed. \
             Download it by hand from the releases page if this keeps happening."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The format ship-zip.ps1 writes: lowercase hex, two spaces, filename.
    const SUMS: &str = "\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  trix-v0.5.0-win-x64.zip
0000000000000000000000000000000000000000000000000000000000000000  SHA256SUMS.txt
";

    /// A scratch file that removes itself even if an assertion above it panics,
    /// and whose name cannot collide with another test's: `process::id()` alone
    /// is only unique per process, but tests in one binary run on multiple
    /// threads, so it is paired here with the thread id too.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(tag: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "trix-verify-{tag}-{:?}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, bytes).expect("write scratch file");
            Self(path)
        }
    }

    impl std::ops::Deref for TempFile {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn the_digest_is_found_by_filename() {
        assert_eq!(
            digest_for(SUMS, "trix-v0.5.0-win-x64.zip").expect("present"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Not "assume it is fine". A sums file with no line for this asset means
    /// the release was assembled wrong, and installing anyway is exactly the
    /// case this module exists to prevent.
    #[test]
    fn a_missing_line_is_a_refusal() {
        assert!(digest_for(SUMS, "trix-v9.9.9-win-x64.zip").is_err());
    }

    #[test]
    fn case_and_stray_whitespace_do_not_matter() {
        let shouty = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855  a.zip\n";
        assert_eq!(
            digest_for(shouty, "a.zip").expect("present"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "comparison is on lowercase hex, so the file's casing cannot cause a false mismatch"
        );
    }

    #[test]
    fn an_empty_file_hashes_to_the_known_empty_digest() {
        let file = TempFile::new("empty", b"");
        assert_eq!(
            sha256_file(&file).expect("hashes"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(check(&file, SUMS, "trix-v0.5.0-win-x64.zip").is_ok());
    }

    #[test]
    fn a_file_that_does_not_match_is_refused_by_name() {
        let file = TempFile::new("bad", b"not empty");
        let error = check(&file, SUMS, "trix-v0.5.0-win-x64.zip").expect_err("must refuse");
        assert!(
            error.contains("trix-v0.5.0-win-x64.zip"),
            "the message has to say which file failed: {error}"
        );
    }

    /// The sums file must use two spaces. A single space is not the published
    /// format and must not be inferred as one, lest we install against a checksum
    /// that was never published.
    #[test]
    fn single_space_separator_is_not_accepted() {
        let single_space_sums = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 trix-v0.5.0-win-x64.zip\n";
        assert!(digest_for(single_space_sums, "trix-v0.5.0-win-x64.zip").is_err());
    }
}
