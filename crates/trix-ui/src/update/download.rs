//! Getting bytes from GitHub, and from nowhere else.
//!
//! Trix made no outbound connection at all before this module. Everything here
//! is deliberately narrow: three hosts, TLS only, a size ceiling, and no
//! request body ever. There is nothing to send -- no identifier, no telemetry,
//! no clip metadata -- and keeping that true is easier when the only function
//! that can reach the network is this one.

use std::io::{Read as _, Write as _};
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
    let mut buffer = vec![0u8; CHUNK];
    let mut received: u64 = 0;
    loop {
        let read = reader.read(&mut buffer).map_err(|e| format!("the download stopped: {e}"))?;
        if read == 0 {
            break;
        }
        received += read as u64;
        // Checked against the bound as it arrives, not only against
        // Content-Length: a server is free to send more than it declared, or
        // to declare nothing at all.
        if received > MAX_BYTES {
            let _ = std::fs::remove_file(dest);
            return Err("the download kept going past the size Trix expects".into());
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("could not write the downloaded update: {e}"))?;
        progress(received, total.max(received));
    }
    file.flush().map_err(|e| format!("could not finish writing the update: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The product is ~3.4 MB. The bound exists so a response that never ends
    /// cannot fill the user's disk, not to be a tight fit.
    #[test]
    fn the_size_bound_is_generous_but_finite() {
        assert!(MAX_BYTES > 32 * 1024 * 1024, "a real release must fit with room to grow");
        assert!(MAX_BYTES <= 128 * 1024 * 1024, "but it must actually bound something");
    }
}
