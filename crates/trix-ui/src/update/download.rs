//! Getting bytes from GitHub, and from nowhere else.
//!
//! Trix made no outbound connection at all before this module. Everything here
//! is deliberately narrow: three hosts, TLS only, a size ceiling, and no
//! request body ever. There is nothing to send -- no identifier, no telemetry,
//! no clip metadata -- and keeping that true is easier when the only function
//! that can reach the network is this one.

use std::io::Write as _;
use std::path::Path;

use super::check::user_agent;

/// Exactly these, matched on the whole host. `github.com` serves
/// `browser_download_url` and redirects to `objects.githubusercontent.com`;
/// `api.github.com` answers the release feed.
const ALLOWED_HOSTS: [&str; 3] = ["api.github.com", "github.com", "objects.githubusercontent.com"];

/// The release zip is about 3.4 MB. This is not a tight bound -- it exists so a
/// response that never ends cannot fill the user's disk while a progress bar
/// climbs forever.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
// The product is ~3.4 MB. The bound exists so a response that never ends
// cannot fill the user's disk, not to be a tight fit -- generous enough for
// a real release to grow into (> 32 MB) but still small enough to actually
// bound something (<= 128 MB), catching someone setting this to, say, a few
// KB or a few GB by mistake. Compile-time rather than a #[test]: both sides
// are known at compile time, so a runtime test would only trip clippy's
// assertions_on_constants lint, and a bad value should fail the build, not
// a test run.
const _: () = assert!(MAX_BYTES > 32 * 1024 * 1024 && MAX_BYTES <= 128 * 1024 * 1024);

/// How much is read per iteration. Large enough that the syscall cost is
/// invisible, small enough that progress moves smoothly on a slow line.
const CHUNK: usize = 64 * 1024;

/// Refuses anything that is not HTTPS to one of [`ALLOWED_HOSTS`].
///
/// Applied to redirect targets too, not only to the URL the release feed
/// named: a redirect is a URL somebody else chose, and the whole point of an
/// allowlist is that it holds for URLs this code did not write.
pub fn allowed(url: &str) -> Result<(), String> {
    let rest = url.strip_prefix("https://").ok_or("updates are only fetched over HTTPS")?;
    // The host ends at the first '/', '?' or '#'. Splitting on all three
    // matters: `https://github.com?x=@evil` has no slash at all.
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Strip userinfo, which is the classic way to make a URL read as one host
    // and resolve as another: `https://github.com@evil.example/`.
    let host = host.rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default().to_ascii_lowercase();

    if ALLOWED_HOSTS.contains(&host.as_str()) {
        Ok(())
    } else {
        Err(format!("updates are not fetched from {host:?}"))
    }
}

/// Redirect hops handled by hand. The real flow needs exactly one
/// (`github.com` to `objects.githubusercontent.com`); this leaves headroom
/// without approaching `ureq`'s own default of ten, which would let a
/// misbehaving server bounce the request in circles rather than fail fast.
const MAX_REDIRECTS: u8 = 5;

/// Fetches `url`, following redirects one hop at a time rather than leaving
/// it to `ureq`'s own redirect handling.
///
/// `ureq` does not know about [`ALLOWED_HOSTS`]: left to its defaults it
/// follows up to ten redirects to whatever host and scheme the server names,
/// which is exactly what the allowlist exists to close, since a redirect is a
/// URL this code did not write. So redirects are turned off at the
/// transport level (`max_redirects(0)`) and each `Location` is checked with
/// [`allowed`] before it is followed.
fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut current = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        allowed(&current)?;
        let response = ureq::get(&current)
            .header("User-Agent", &user_agent())
            .config()
            .max_redirects(0)
            .build()
            .call()
            .map_err(|e| format!("could not reach GitHub: {e}"))?;

        if !response.status().is_redirection() {
            return if response.status().is_success() {
                Ok(response)
            } else {
                Err(format!("GitHub answered with {}", response.status()))
            };
        }

        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or("GitHub redirected without saying where to")?;
        current = location.to_string();
    }
    Err("the download redirected too many times".into())
}

/// For the release feed and `SHA256SUMS.txt`, both small.
pub fn fetch_text(url: &str) -> Result<String, String> {
    let mut response = get(url)?;
    response.body_mut().read_to_string().map_err(|e| format!("could not read GitHub's answer: {e}"))
}

/// Streams `url` to `dest`, calling `progress(received, total)` as it goes.
///
/// Written to a file rather than held in memory, and bounded, so neither a
/// large release nor an endless response is a memory problem.
pub fn fetch_to_file(url: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    let mut response = get(url)?;

    let total: u64 = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if total > MAX_BYTES {
        return Err(format!("the download is {total} bytes, which is larger than Trix expects"));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create the update folder: {e}"))?;
    }
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("could not write the downloaded update: {e}"))?;

    let mut reader = response.body_mut().as_reader();
    stream_to_file(dest, &mut file, &mut reader, total, progress)
}

/// Runs the copy loop and deletes `dest` if it fails partway through, for
/// any reason -- a dropped connection and a full disk leave a half-written
/// file exactly as surely as the size bound does. The loop is wrapped in an
/// immediately-invoked closure rather than split into its own `fn`, so its
/// `?` early-returns keep working as `?` instead of becoming a `match` at
/// every call site, while the cleanup below still runs exactly once no
/// matter which of those early returns fired -- a future error path added
/// inside the loop is covered automatically, the way three of the four
/// existing ones originally were not.
///
/// `reader` is a trait object rather than `Body`, and `file`/`dest` are
/// passed in rather than opened here, so this whole thing -- cleanup
/// included -- can be driven by a test with synthetic data instead of a
/// network connection.
fn stream_to_file(
    dest: &Path,
    file: &mut std::fs::File,
    reader: &mut dyn std::io::Read,
    total: u64,
    progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    let result: Result<(), String> = (|| {
        let mut buffer = vec![0u8; CHUNK];
        let mut received: u64 = 0;
        loop {
            let read =
                reader.read(&mut buffer).map_err(|e| format!("the download stopped: {e}"))?;
            if read == 0 {
                break;
            }
            received += read as u64;
            // Checked against the bound as it arrives, not only against
            // Content-Length: a server is free to send more than it declared, or
            // to declare nothing at all.
            if received > MAX_BYTES {
                return Err("the download kept going past the size Trix expects".into());
            }
            file.write_all(&buffer[..read])
                .map_err(|e| format!("could not write the downloaded update: {e}"))?;
            progress(received, total.max(received));
        }
        file.flush().map_err(|e| format!("could not finish writing the update: {e}"))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch path that removes itself even if an assertion above it
    /// panics, and whose name cannot collide with another test's:
    /// `process::id()` alone is only unique per process, but tests in one
    /// binary run on multiple threads, so it is paired here with the thread
    /// id too. Mirrors the `TempFile` in `verify.rs`'s test module rather
    /// than importing it -- that one is private to that module, and sharing
    /// it would mean making a test helper `pub` across the crate for one
    /// file. This variant only names the path; unlike `verify.rs`'s, it does
    /// not create the file, because these tests need to assert on cases
    /// where `stream_to_file` is the one deciding whether the file exists
    /// afterward.
    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn named(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "trix-download-{tag}-{:?}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);
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

    /// Yields a few bytes once, then fails every call after -- models the
    /// common case finding 1 was about, a connection dropped mid-download,
    /// not the size bound.
    struct DropsMidStream {
        handed_out: bool,
    }

    impl std::io::Read for DropsMidStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.handed_out {
                return Err(std::io::Error::other("connection reset"));
            }
            self.handed_out = true;
            let n = buf.len().min(4);
            buf[..n].copy_from_slice(&b"data"[..n]);
            Ok(n)
        }
    }

    /// Always claims to have filled the caller's buffer. `CHUNK` is 64 KiB,
    /// so this crosses `MAX_BYTES` (64 MiB) in about a thousand calls
    /// without the test holding 64 MB of real data anywhere -- the "small
    /// buffer" is this struct's own (empty) state, not the chunk size,
    /// which is `stream_to_file`'s to decide either way.
    struct AlwaysFullBuffer;

    impl std::io::Read for AlwaysFullBuffer {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
    }

    #[test]
    fn a_dropped_connection_leaves_no_partial_file() {
        let dest = TempFile::named("dropped-connection");
        let mut file = std::fs::File::create(&*dest).expect("create scratch file");
        let mut reader = DropsMidStream { handed_out: false };

        let result = stream_to_file(&dest, &mut file, &mut reader, 0, &|_, _| {});

        assert!(result.is_err(), "a dropped connection must be reported as an error");
        assert!(!dest.exists(), "no partial file must be left when the download drops");
    }

    #[test]
    fn an_oversized_stream_leaves_no_partial_file_and_reports_the_existing_error() {
        let dest = TempFile::named("oversized-stream");
        let mut file = std::fs::File::create(&*dest).expect("create scratch file");
        let mut reader = AlwaysFullBuffer;

        let result = stream_to_file(&dest, &mut file, &mut reader, 0, &|_, _| {});

        let error = result.expect_err("a stream past MAX_BYTES must be refused");
        assert_eq!(error, "the download kept going past the size Trix expects");
        assert!(!dest.exists(), "no partial file must be left when the download is oversized");
    }

    #[test]
    fn a_normal_stream_is_written_whole_and_progress_ends_at_the_total_received() {
        let dest = TempFile::named("normal-stream");
        let mut file = std::fs::File::create(&*dest).expect("create scratch file");
        let payload = b"the entire contents of a small update file";
        let mut reader: &[u8] = payload;
        let last_progress = std::cell::Cell::new((0u64, 0u64));

        let result = stream_to_file(
            &dest,
            &mut file,
            &mut reader,
            payload.len() as u64,
            &|received, total| {
                last_progress.set((received, total));
            },
        );

        assert!(result.is_ok(), "a normal stream must succeed");
        assert_eq!(std::fs::read(&*dest).expect("read back scratch file"), payload);
        assert_eq!(last_progress.get(), (payload.len() as u64, payload.len() as u64));
    }

    #[test]
    fn the_three_hosts_the_release_flow_uses_are_allowed() {
        // api.github.com answers the feed; browser_download_url points at
        // github.com and redirects to objects.githubusercontent.com.
        assert!(allowed("https://api.github.com/repos/tnhnblgl/trix/releases/latest").is_ok());
        assert!(allowed("https://github.com/tnhnblgl/trix/releases/download/v0.5.0/a.zip").is_ok());
        assert!(
            allowed("https://objects.githubusercontent.com/github-production-release/1/2").is_ok()
        );
    }

    /// A suffix match would accept every one of these. The check is on the
    /// host component, whole and exact.
    #[test]
    fn lookalike_hosts_are_refused() {
        for url in [
            "https://github.com.evil.example/x.zip",
            "https://evil.example/github.com/x.zip",
            "https://notgithub.com/x.zip",
            "https://api.github.com.evil.example/x",
        ] {
            assert!(allowed(url).is_err(), "{url} must not be reachable");
        }
    }

    /// TLS is not optional: plain HTTP would let anyone on the path swap the
    /// zip, and the checksum with it.
    #[test]
    fn plain_http_is_refused_even_to_an_allowed_host() {
        assert!(allowed("http://github.com/tnhnblgl/trix/releases/download/v0.5.0/a.zip").is_err());
    }

    #[test]
    fn a_url_that_is_not_a_url_is_refused() {
        assert!(allowed("not a url").is_err());
        assert!(allowed("file:///C:/Windows/System32/cmd.exe").is_err());
    }
}
