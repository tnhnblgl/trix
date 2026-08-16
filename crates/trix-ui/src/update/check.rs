//! Asking GitHub whether there is a newer Trix, and deciding whether the
//! answer is usable.
//!
//! Parsing is a free function over a `&str` so every rule below is testable
//! without a network, a server, or a fixture file. The one function that
//! touches the network does nothing but fetch the bytes.

use serde_json::Value;

/// The GitHub account and repository Trix updates itself from, written once.
///
/// A macro rather than a `const` because `concat!` takes literals, and the two
/// constants below have to be cut from the same one: [`RELEASE_API`] is where a
/// release is *discovered*, [`RELEASE_DOWNLOAD_PREFIX`] is where its files may
/// be *fetched from*, and two independent strings that merely agree today is
/// precisely the drift worth ruling out — a repository renamed in one and not
/// the other would leave Trix taking one repository's word for what another
/// repository's binaries are. Change it here and both follow.
macro_rules! repo {
    () => {
        "tnhnblgl/trix"
    };
}

/// `/releases/latest` rather than `/releases`: GitHub already excludes drafts
/// and prereleases from it, so a prerelease cannot reach users by accident.
pub const RELEASE_API: &str = concat!("https://api.github.com/repos/", repo!(), "/releases/latest");

/// Where a file has to live before Trix will download it, check it and run it.
///
/// [`super::download::allowed`] is the other half of this and cannot replace
/// it: it constrains the *host*, and it has to, because it is also applied to
/// redirect targets — URLs nobody here wrote — and the real download hops from
/// `github.com` to `objects.githubusercontent.com`. That leaves `github.com`
/// allowlisted whole, which is every account and every repository on it.
///
/// `update_install` takes its [`Release`] as a command argument, so the struct
/// it acts on is whatever the webview handed over, not necessarily what
/// [`newer_release`] built. Without this, anything with webview execution could
/// name a `zip_url` and a matching `sums_url` in its own GitHub releases: the
/// host allowlist passes, `verify::check` passes because the checksums file is
/// theirs too, and `verify_payload_version` then *runs* the `trix.exe` inside.
/// The trust anchor for this feature is this account's releases, and this is
/// the constant that says so.
pub const RELEASE_DOWNLOAD_PREFIX: &str =
    concat!("https://github.com/", repo!(), "/releases/download/");

/// GitHub rejects API requests that send no User-Agent.
pub fn user_agent() -> String {
    format!("trix/{}", env!("CARGO_PKG_VERSION"))
}

/// A release that is newer than what is running, and complete enough to install.
///
/// `Deserialize` as well as `Serialize`, because this makes a whole round trip
/// through the webview: `update_check` hands one out, the banner holds it while
/// the user decides, and `update_install` takes the same object back as a
/// command argument — which Tauri can only deliver by deserialising it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    pub version: String,
    pub notes_url: String,
    pub zip_url: String,
    pub sums_url: String,
    pub size: u64,
}

impl Release {
    /// Refuses a release whose files do not come from this account's releases.
    ///
    /// Both URLs, in one place, so no caller can check the zip and forget the
    /// checksums file — which would be the worse half to skip, since the
    /// checksums are the only thing the zip is measured against, and an
    /// attacker who supplies both is verifying their download against their
    /// own digest. See [`RELEASE_DOWNLOAD_PREFIX`] for what this is closing.
    pub fn assets_are_ours(&self) -> Result<(), String> {
        for url in [&self.zip_url, &self.sums_url] {
            if !from_our_releases(url) {
                return Err(format!(
                    "the update points at {url}, which is not a file published on Trix's own \
                     releases page, so nothing was downloaded"
                ));
            }
        }
        Ok(())
    }
}

/// Whether `url` is a file published under this repository's releases.
///
/// A prefix match on the whole of [`RELEASE_DOWNLOAD_PREFIX`], which ends at
/// `/download/` and so cannot be satisfied by a repository whose name merely
/// starts with this one's: `tnhnblgl/trix-evil` fails on the `/releases` that
/// has to come next. A prefix found anywhere later in the URL is not a prefix
/// and never matches.
///
/// The `..` rule is the other half, and a prefix check is not sound without
/// it: a URL is resolved against its own path before it is fetched, so
/// `.../releases/download/../../someone-else/...` is a URL that starts with
/// the prefix and asks for a file outside it. Nothing `ship-zip.ps1` publishes
/// contains one, so refusing them outright costs nothing.
fn from_our_releases(url: &str) -> bool {
    url.starts_with(RELEASE_DOWNLOAD_PREFIX) && !url.contains("..")
}

/// `Ok(None)` means "nothing to offer" and is not a problem: it covers the
/// common case (already current) and the awkward one (a release whose assets
/// are still uploading). `Err` is reserved for a body that could not be
/// understood at all, which is worth a log line.
pub fn newer_release(body: &str, current: &str) -> Result<Option<Release>, String> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| format!("release feed was not JSON: {e}"))?;

    let tag = json.get("tag_name").and_then(Value::as_str).ok_or("release feed has no tag_name")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);

    let latest = semver::Version::parse(version)
        .map_err(|e| format!("release tag {tag:?} is not a version: {e}"))?;
    let running = semver::Version::parse(current)
        .map_err(|e| format!("this build's own version {current:?} is not a version: {e}"))?;
    if latest <= running {
        return Ok(None);
    }

    let assets = json.get("assets").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let zip_name = format!("trix-v{version}-win-x64.zip");

    // Both assets or neither. A zip with no digest beside it cannot be
    // verified, and an unverifiable download is not an update -- it is a
    // failure waiting to happen halfway through the swap.
    let Some(zip) = find_asset(assets, &zip_name) else { return Ok(None) };
    let Some(sums) = find_asset(assets, "SHA256SUMS.txt") else { return Ok(None) };

    Ok(Some(Release {
        version: version.to_string(),
        notes_url: json
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or("https://github.com/tnhnblgl/trix/releases")
            .to_string(),
        zip_url: zip.0,
        sums_url: sums.0,
        size: zip.1,
    }))
}

/// `(browser_download_url, size)` for an asset with exactly this name.
fn find_asset(assets: &[Value], name: &str) -> Option<(String, u64)> {
    assets.iter().find(|a| a.get("name").and_then(Value::as_str) == Some(name)).map(|a| {
        (
            a.get("browser_download_url").and_then(Value::as_str).unwrap_or_default().to_string(),
            a.get("size").and_then(Value::as_u64).unwrap_or_default(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `releases/latest` body. Real ones carry ~40 more fields; the
    /// parser must ignore all of them, so the fixture carries only what is read.
    fn body(tag: &str, assets: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/tnhnblgl/trix/releases/tag/{tag}","assets":[{assets}]}}"#
        )
    }

    fn asset(name: &str, size: u64) -> String {
        format!(
            r#"{{"name":"{name}","size":{size},"browser_download_url":"https://github.com/tnhnblgl/trix/releases/download/v0.5.0/{name}"}}"#
        )
    }

    fn full_release(tag: &str) -> String {
        body(
            tag,
            &format!(
                "{},{}",
                asset("trix-v0.5.0-win-x64.zip", 3_400_000),
                asset("SHA256SUMS.txt", 96)
            ),
        )
    }

    #[test]
    fn a_newer_tag_is_an_update() {
        let found = newer_release(&full_release("v0.5.0"), "0.4.0").expect("parses");
        let release = found.expect("0.5.0 is newer than 0.4.0");
        assert_eq!(release.version, "0.5.0", "the leading v is not part of the version");
        assert_eq!(release.size, 3_400_000);
        assert!(release.zip_url.ends_with("trix-v0.5.0-win-x64.zip"));
        assert!(release.sums_url.ends_with("SHA256SUMS.txt"));
    }

    #[test]
    fn the_same_version_is_not_an_update() {
        assert!(newer_release(&full_release("v0.4.0"), "0.4.0").expect("parses").is_none());
    }

    /// Downgrades are never offered. A user who deliberately kept an older
    /// build must not be nagged to install one they already left, and a
    /// mistakenly re-pointed `latest` must not roll everybody backwards.
    #[test]
    fn an_older_tag_is_not_an_update() {
        assert!(newer_release(&full_release("v0.3.9"), "0.4.0").expect("parses").is_none());
    }

    #[test]
    fn a_tag_without_its_v_still_works() {
        assert!(newer_release(&full_release("0.5.0"), "0.4.0").expect("parses").is_some());
    }

    /// GitHub publishes the release before the attached files finish
    /// uploading, and the user creates releases by hand. A check landing in
    /// that window must read as "nothing to do" -- the alternative is a banner
    /// offering a download that does not exist, which is the one failure here
    /// the user would notice and could do nothing about.
    #[test]
    fn a_release_whose_assets_are_still_uploading_is_not_an_update() {
        assert!(newer_release(&body("v0.5.0", ""), "0.4.0").expect("parses").is_none());

        let zip_only = body("v0.5.0", &asset("trix-v0.5.0-win-x64.zip", 3_400_000));
        assert!(
            newer_release(&zip_only, "0.4.0").expect("parses").is_none(),
            "a zip with no SHA256SUMS.txt cannot be verified, so it is not offered"
        );
    }

    /// The asset name carries the version, so a mismatched pair means the
    /// release was assembled wrong. Offering it would download one version
    /// while promising another.
    #[test]
    fn an_asset_named_for_a_different_version_is_refused() {
        let wrong = body(
            "v0.5.0",
            &format!("{},{}", asset("trix-v0.4.9-win-x64.zip", 10), asset("SHA256SUMS.txt", 96)),
        );
        assert!(newer_release(&wrong, "0.4.0").expect("parses").is_none());
    }

    #[test]
    fn a_malformed_tag_is_an_error_not_a_silent_no() {
        assert!(newer_release(&full_release("nightly"), "0.4.0").is_err());
    }

    /// A release carrying whatever pair of URLs a test wants to hand
    /// `update_install`, which is exactly what the webview can do.
    fn release_with(zip_url: &str, sums_url: &str) -> Release {
        Release {
            version: "0.5.0".to_string(),
            notes_url: "https://github.com/tnhnblgl/trix/releases/tag/v0.5.0".to_string(),
            zip_url: zip_url.to_string(),
            sums_url: sums_url.to_string(),
            size: 3_400_000,
        }
    }

    /// The URLs a real release actually carries, so the guard cannot be the
    /// kind that refuses everything and passes its negative tests.
    #[test]
    fn the_urls_a_real_release_carries_are_accepted() {
        let release = newer_release(&full_release("v0.5.0"), "0.4.0")
            .expect("parses")
            .expect("0.5.0 is newer");
        release.assets_are_ours().expect("a release built from this repository's feed must pass");
    }

    /// The host allowlist in `download.rs` cannot make this distinction: every
    /// URL below is on `github.com` and passes it. What separates them is the
    /// account and the repository, which is the whole trust anchor -- an
    /// attacker's own release, checksummed against their own SHA256SUMS.txt,
    /// would otherwise be downloaded, verified, and run.
    #[test]
    fn a_release_hosted_by_another_account_or_repository_is_refused() {
        for url in [
            "https://github.com/attacker/trix/releases/download/v0.5.0/trix-v0.5.0-win-x64.zip",
            "https://github.com/tnhnblgl/notes/releases/download/v0.5.0/trix-v0.5.0-win-x64.zip",
        ] {
            let error = release_with(url, url)
                .assets_are_ours()
                .expect_err("another account's release must not be installable");
            assert!(error.contains(url), "the message must name the URL it refused: {error}");
        }
    }

    /// The shapes a prefix check gets wrong when it is written carelessly: a
    /// repository whose name starts with this one's, the prefix appearing
    /// somewhere other than the start, and a path that walks back out of the
    /// prefix it opened with.
    #[test]
    fn urls_that_only_look_like_this_repositorys_releases_are_refused() {
        for url in [
            // Same account, and `trix` really is a prefix of `trix-evil` --
            // but not of `trix/releases/download/`.
            "https://github.com/tnhnblgl/trix-evil/releases/download/v0.5.0/trix.zip",
            // The legitimate prefix, further along the path rather than at the
            // start of it.
            "https://github.com/attacker/repo/raw/main/https://github.com/tnhnblgl/trix/releases/download/v0.5.0/trix.zip",
            "https://evil.example/https://github.com/tnhnblgl/trix/releases/download/v0.5.0/trix.zip",
            // Starts with the prefix and then leaves it: a URL is resolved
            // against its own path before it is fetched.
            "https://github.com/tnhnblgl/trix/releases/download/../../attacker/trix/releases/download/v0.5.0/trix.zip",
            // Right repository, but not a release download.
            "https://github.com/tnhnblgl/trix/raw/main/trix.zip",
            // Scheme is part of the prefix, so this cannot pass either.
            "http://github.com/tnhnblgl/trix/releases/download/v0.5.0/trix.zip",
        ] {
            assert!(!from_our_releases(url), "{url} must not read as a Trix release");
        }
    }

    /// The zip is the payload, but the checksums file is what the payload is
    /// measured against, so a release that pairs a genuine zip URL with an
    /// attacker's sums URL must be refused just as firmly.
    #[test]
    fn the_checksums_url_is_checked_as_well_as_the_zip() {
        const OURS: &str =
            "https://github.com/tnhnblgl/trix/releases/download/v0.5.0/trix-v0.5.0-win-x64.zip";
        const OUR_SUMS: &str =
            "https://github.com/tnhnblgl/trix/releases/download/v0.5.0/SHA256SUMS.txt";
        const THEIRS: &str =
            "https://github.com/attacker/trix/releases/download/v0.5.0/SHA256SUMS.txt";

        release_with(OURS, OUR_SUMS).assets_are_ours().expect("both from this repository");
        let error = release_with(OURS, THEIRS)
            .assets_are_ours()
            .expect_err("a genuine zip beside someone else's checksums is not a Trix release");
        assert!(error.contains(THEIRS), "the sums URL is the one at fault here: {error}");
    }

    /// GitHub rejects API requests with no User-Agent.
    #[test]
    fn the_user_agent_names_the_product_and_version() {
        let ua = user_agent();
        assert!(ua.starts_with("trix/"), "GitHub refuses requests without a User-Agent: {ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
    }
}
