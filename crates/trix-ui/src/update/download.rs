//! Getting bytes from GitHub, and from nowhere else.
//!
//! Trix made no outbound connection at all before this module. Everything here
//! is deliberately narrow: GitHub's hosts only, TLS only, a size ceiling, and
//! no request body ever. There is nothing to send -- no identifier, no telemetry,
//! no clip metadata -- and keeping that true is easier when the only function
//! that can reach the network is this one.

use std::io::Write as _;
use std::path::Path;
use std::sync::OnceLock;

use super::check::user_agent;

/// Exactly these, matched on the whole host. `api.github.com` answers the
/// release feed; `github.com` serves `browser_download_url` and redirects to
/// wherever GitHub is keeping release bytes this year.
const ALLOWED_HOSTS: [&str; 2] = ["api.github.com", "github.com"];

/// Where that redirect lands, matched as a suffix rather than as a name.
///
/// This used to be the single host `objects.githubusercontent.com`, and in
/// 0.5.2 GitHub started answering `browser_download_url` with a redirect to
/// `release-assets.githubusercontent.com` instead. Every Trix that shipped an
/// updater refused it -- "updates are not fetched from
/// `release-assets.githubusercontent.com`" -- so the feature that exists to
/// spare people a manual download made one necessary for everybody at once.
/// Pinning the label has now cost that twice, and a rename is GitHub's to make
/// without telling anyone.
///
/// Widening this costs nothing that was being relied on, and it is worth being
/// precise about why, because a shorter allowlist *looks* safer. This list
/// never was the trust anchor: [`super::check::RELEASE_DOWNLOAD_PREFIX`] is,
/// and it pins both the zip and the checksums file to this account's own
/// releases before a request is made at all. What this list does is stop a
/// redirect from walking the download off GitHub's infrastructure, and
/// `githubusercontent.com` is GitHub's infrastructure whatever they put in
/// front of it. Note that the previous list already allowed `github.com`
/// whole, which is every account and every repository on it -- so this is not
/// where the strength was.
///
/// The leading dot is the whole of the safety here: `.githubusercontent.com`
/// cannot be satisfied by `evilgithubusercontent.com`, which a bare
/// `ends_with("githubusercontent.com")` would wave through.
const ALLOWED_SUFFIX: &str = ".githubusercontent.com";

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

/// Refuses anything that is not HTTPS to one of [`ALLOWED_HOSTS`] or to a
/// host under [`ALLOWED_SUFFIX`].
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

    if ALLOWED_HOSTS.contains(&host.as_str()) || host.ends_with(ALLOWED_SUFFIX) {
        Ok(())
    } else {
        Err(format!("updates are not fetched from {host:?}"))
    }
}

/// Redirect hops handled by hand. The real flow needs exactly one
/// (`github.com` to a `githubusercontent.com` host); this leaves headroom
/// without approaching `ureq`'s own default of ten, which would let a
/// misbehaving server bounce the request in circles rather than fail fast.
const MAX_REDIRECTS: u8 = 5;

/// The one agent every request goes through, built once.
///
/// It exists to name the TLS backend. `ureq`'s `TlsProvider` is an enum whose
/// `Default` is `Rustls`, and that default is what you get from
/// `ureq::get(..)` and from any agent that does not say otherwise -- the
/// Cargo feature has no say in it. This crate compiles `ureq` with
/// `native-tls` and not `rustls`, so the default asks for a backend that was
/// never linked, and `ureq` answers that by panicking on the first `https`
/// request. With `panic = "abort"` in the release profile, the panic is the
/// end of the process: the window vanishes with no error and no dialog, and
/// since the update check runs when the app opens, it takes the app with it.
/// That shipped in 0.4.0 and 0.5.0.
///
/// `max_redirects(0)` lives here for the same reason -- one place, applied to
/// every request -- and is explained at [`get`].
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build(),
            )
            .max_redirects(0)
            .build()
            .into()
    })
}

/// Fetches `url`, following redirects one hop at a time rather than leaving
/// it to `ureq`'s own redirect handling.
///
/// `ureq` does not know about [`ALLOWED_HOSTS`]: left to its defaults it
/// follows up to ten redirects to whatever host and scheme the server names,
/// which is exactly what the allowlist exists to close, since a redirect is a
/// URL this code did not write. So redirects are turned off at the transport
/// level -- `max_redirects(0)`, set once on [`agent`] -- and each `Location`
/// is checked with [`allowed`] before it is followed.
fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut current = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        allowed(&current)?;
        let response = agent()
            .get(&current)
            .header("User-Agent", &user_agent())
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
    fn the_hosts_the_release_flow_uses_are_allowed() {
        // api.github.com answers the feed; browser_download_url points at
        // github.com and redirects to a content host.
        assert!(allowed("https://api.github.com/repos/tnhnblgl/trix/releases/latest").is_ok());
        assert!(allowed("https://github.com/tnhnblgl/trix/releases/download/v0.5.0/a.zip").is_ok());
        assert!(
            allowed("https://objects.githubusercontent.com/github-production-release/1/2").is_ok()
        );
    }

    /// The 0.5.2 break, as a test. GitHub moved release assets to this host,
    /// and every Trix with an updater refused the redirect and told the user
    /// to download by hand -- which is the one thing the updater exists to
    /// avoid. If this ever fails again, so has the updater, for everyone.
    #[test]
    fn the_host_github_actually_redirects_release_downloads_to_is_allowed() {
        assert!(
            allowed(
                "https://release-assets.githubusercontent.com/github-production-release-asset/1/2"
            )
            .is_ok()
        );
    }

    /// The dot in [`ALLOWED_SUFFIX`] is what separates these from the real
    /// thing, and every one of them would pass a bare `ends_with`.
    #[test]
    fn lookalike_hosts_are_refused() {
        for url in [
            "https://github.com.evil.example/x.zip",
            "https://evil.example/github.com/x.zip",
            "https://notgithub.com/x.zip",
            "https://api.github.com.evil.example/x",
            "https://evilgithubusercontent.com/x.zip",
            "https://githubusercontent.com.evil.example/x.zip",
            "https://release-assets.githubusercontent.com.evil.example/x.zip",
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

    /// The bug that shipped in 0.4.0 and 0.5.0. `ureq`'s `TlsProvider`
    /// defaults to `Rustls` regardless of which TLS feature the crate was
    /// built with, so an agent that never names a provider asks for a backend
    /// that is not linked, and `ureq` panics on the first `https` request --
    /// which under `panic = "abort"` ends the process with no window and no
    /// message. Only a real network call reaches that code, and there is no
    /// network in a test run, so the assertion is on the configuration the
    /// call would use.
    #[test]
    fn the_agent_asks_for_the_tls_backend_that_is_linked() {
        assert_eq!(agent().config().tls_config().provider(), ureq::tls::TlsProvider::NativeTls);
    }

    /// `get` turns redirects off at the transport and follows each `Location`
    /// itself, so [`allowed`] sees every hop. That setting lives on the shared
    /// agent now rather than on each request, where it is easy to lose in a
    /// later edit without any test noticing.
    #[test]
    fn the_agent_does_not_follow_redirects_by_itself() {
        assert_eq!(agent().config().max_redirects(), 0);
    }

    /// The gate the two tests above only approximate. They read the agent's
    /// settings; this one makes the TLS handshake actually happen, which is
    /// the only thing that would have caught the shipped panic. Ignored
    /// because it needs the internet and GitHub's rate limit is per-IP:
    ///
    /// ```text
    /// cargo test -p trix-ui --bin trix-ui -- --ignored the_real_release_feed
    /// ```
    ///
    /// Run it before shipping any release. A panic here fails as a panic,
    /// not as a returned `Err` -- that is the difference being tested.
    #[test]
    #[ignore = "reaches the real api.github.com"]
    fn the_real_release_feed_can_actually_be_fetched() {
        let body = fetch_text(crate::update::check::RELEASE_API)
            .expect("GitHub should answer the release feed");
        assert!(body.contains("\"tag_name\""), "not a release payload: {body:.200}");
    }

    /// The 0.5.2 gate, and the one that would have caught it. Everything else
    /// about that break passed: the feed was fetched, the release was parsed,
    /// the banner appeared, the button worked. It failed on the hop from
    /// `github.com` to the content host, which is a thing only a real request
    /// does.
    ///
    /// ```text
    /// cargo test -p trix-ui --bin trix-ui -- --ignored the_real_release_download
    /// ```
    ///
    /// Run it before shipping any release, next to the feed test above. It
    /// deliberately follows the chain to a `200` and reads the first bytes
    /// rather than the whole zip -- the redirect is what is being tested, and
    /// the payload has its own checksum gate further along.
    #[test]
    #[ignore = "reaches the real github.com and downloads from a release"]
    fn the_real_release_download_survives_githubs_redirect() {
        let feed = fetch_text(crate::update::check::RELEASE_API).expect("release feed");
        let tag = feed
            .split("\"tag_name\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a tag_name in the feed");
        let url = format!(
            "{}{tag}/trix-{tag}-win-x64.zip",
            crate::update::check::RELEASE_DOWNLOAD_PREFIX
        );

        let mut response = get(&url).expect("the release zip should be reachable");
        assert!(response.status().is_success(), "{url} answered {}", response.status());

        let mut head = [0u8; 4];
        std::io::Read::read_exact(&mut response.body_mut().as_reader(), &mut head)
            .expect("the redirect should end at real bytes");
        assert_eq!(&head[..2], b"PK", "not a zip: {head:?}");
    }
}
