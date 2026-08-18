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

/// The releases page, and the prefix every page Trix will open in a browser
/// has to start with.
///
/// Two links in the UI point here — "Download it by hand" on a failed update,
/// and "What's new" beside an offered one — and the second is a `notes_url`
/// that arrived from the release feed, so it is a string from the network
/// being handed to `ShellExecuteW`. Matching on this prefix keeps that to
/// pages of this account's own releases: a `notes_url` naming somewhere else
/// opens nothing rather than opening whatever it named.
pub const RELEASES_URL: &str = concat!("https://github.com/", repo!(), "/releases");

/// Where a file has to live before Trix will download it, check it and run it.
///
/// [`super::download::allowed`] is the other half of this and cannot replace
/// it: it constrains the *host*, and it has to, because it is also applied to
/// redirect targets — URLs nobody here wrote — and the real download hops from
/// `github.com` to a `githubusercontent.com` host. That leaves `github.com`
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

/// The checksums file every release publishes beside the zip.
///
/// Written once because it is two things at once: the asset [`newer_release`]
/// insists on finding in the feed, and the last path segment
/// [`from_our_releases`] requires the sums URL to end in.
const SUMS_NAME: &str = "SHA256SUMS.txt";

/// The zip a release of `version` publishes.
///
/// One spelling, because three separate things key off it: the asset
/// [`newer_release`] looks for, the final path segment the zip URL has to end
/// in, and the filename `verify::check` looks up in `SHA256SUMS.txt`. It is
/// also the name the download is written under, which is why nothing may build
/// it out of a `version` that has not been through
/// [`version_can_name_a_file`] first.
fn zip_name(version: &str) -> String {
    format!("trix-v{version}-win-x64.zip")
}

/// GitHub rejects API requests that send no User-Agent.
pub fn user_agent() -> String {
    format!("trix/{}", env!("CARGO_PKG_VERSION"))
}

/// Whether `version` is made only of what a version number is made of.
///
/// `update_install` takes its [`Release`] as a command argument, so `version`
/// is whatever the webview passed in — and it is the field that reaches the
/// filesystem: it is interpolated into [`zip_name`], joined onto the staging
/// directory, and handed to a `create_dir_all` and a `File::create`. A
/// `version` carrying `..\` or a drive letter walks that path straight out of
/// the staging folder and writes wherever it likes.
///
/// Deliberately not "does semver parse it". [`is_newer`] asks that a moment
/// later and its answer happens to exclude a path separator, but only as an
/// accident of what a version number looks like — not because anything decided
/// a separator was unsafe here. The rule that keeps a filename a filename is
/// worth stating in its own right.
fn version_can_name_a_file(version: &str) -> bool {
    !version.is_empty()
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Whether `candidate` is a version worth moving to from `current`.
///
/// One comparison, asked in two places. [`newer_release`] asks it about the tag
/// on GitHub's `latest`, so a downgrade is never offered; [`Release::asset_to_install`]
/// asks it again about the release the webview handed back, so a downgrade
/// cannot be installed either — the second is the one whose input an attacker
/// gets to choose, and pointing it at a *genuine* older release is otherwise a
/// silent, fully-verified downgrade. Two spellings of "is this newer" would be
/// two chances to get the direction wrong.
fn is_newer(candidate: &str, current: &str) -> Result<bool, String> {
    let offered = semver::Version::parse(candidate)
        .map_err(|e| format!("release version {candidate:?} is not a version: {e}"))?;
    let running = semver::Version::parse(current)
        .map_err(|e| format!("this build's own version {current:?} is not a version: {e}"))?;
    Ok(offered > running)
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
    /// Everything about a release that has to be true before a byte of it is
    /// fetched, and the filename to fetch.
    ///
    /// `update_install` takes its `Release` as a command argument, so every
    /// field below is whatever the webview passed in — not necessarily what
    /// [`newer_release`] built, and not necessarily anything GitHub ever
    /// published. Three separate claims have to be established: that the
    /// version is a version and not a path, that it is actually an upgrade,
    /// and that both files come from this account's releases.
    ///
    /// It hands back the asset filename rather than `()` because that is what
    /// stops the three from being skipped. The name the download is written
    /// under, the name the checksum is looked up by and the name the zip URL
    /// has to end in are all the same string, and it does not exist until this
    /// function has produced it.
    pub fn asset_to_install(&self, running: &str) -> Result<String, String> {
        if !version_can_name_a_file(&self.version) {
            return Err(format!(
                "the update calls itself version {:?}, which is not a version number, so nothing \
                 was downloaded",
                self.version
            ));
        }
        if !is_newer(&self.version, running)? {
            return Err(format!(
                "the update offers Trix {}, which is not newer than the {running} already \
                 installed, so nothing was downloaded",
                self.version
            ));
        }
        let zip = zip_name(&self.version);
        self.assets_are_ours(&zip)?;
        Ok(zip)
    }

    /// Refuses a release whose files do not come from this account's releases.
    ///
    /// Both URLs, in one place, so no caller can check the zip and forget the
    /// checksums file — which would be the worse half to skip, since the
    /// checksums are the only thing the zip is measured against, and an
    /// attacker who supplies both is verifying their download against their
    /// own digest. See [`RELEASE_DOWNLOAD_PREFIX`] for what this is closing.
    ///
    /// Private, and reached only through [`Release::asset_to_install`]: the
    /// filename each URL has to end in is derived from a `version` that has
    /// been checked by then, and a caller that could ask this question on its
    /// own could ask it with a name of its own choosing.
    fn assets_are_ours(&self, zip: &str) -> Result<(), String> {
        for (url, file) in [(&self.zip_url, zip), (&self.sums_url, SUMS_NAME)] {
            if !from_our_releases(url, file) {
                return Err(format!(
                    "the update points at {url}, which is not a file published on Trix's own \
                     releases page, so nothing was downloaded"
                ));
            }
        }
        Ok(())
    }
}

/// Whether `url` is `file`, published under this repository's releases.
///
/// A prefix match on the whole of [`RELEASE_DOWNLOAD_PREFIX`], which ends at
/// `/download/` and so cannot be satisfied by a repository whose name merely
/// starts with this one's: `tnhnblgl/trix-evil` fails on the `/releases` that
/// has to come next. A prefix found anywhere later in the URL is not a prefix
/// and never matches.
///
/// The rest of this function is about the gap between a string prefix and a
/// path prefix, because a URL is resolved against its own path before it is
/// fetched: `.../releases/download/../../someone-else/...` starts with the
/// prefix and asks for a file outside it. Refusing `..` is the obvious half,
/// and on its own it refuses one *spelling*. `%2e%2e` contains no dot at all,
/// starts with the prefix, and passes [`super::download::allowed`] because the
/// host really is `github.com` — and then resolves to whatever the origin
/// decodes it to. A backslash is the same trick aimed at a different
/// normaliser. So the remainder is constrained rather than the spellings
/// blacklisted: no `..`, no `%`, no `\`, no `?`, no `#`, and a final path
/// segment that is exactly the file this release claims to be. Nothing
/// `ship-zip.ps1` publishes contains any of them, so refusing them costs
/// nothing.
///
/// The last three go together. A final-segment rule is only worth anything
/// while the end of the string is also the end of what gets fetched, and a
/// query string or a fragment is exactly where that stops being true:
/// `…/download/v0.3.0/trix-v0.3.0-win-x64.zip#/trix-v0.5.0-win-x64.zip` has no
/// `..`, no `%` and no `\`, starts with the prefix, ends in the name a 0.5.0
/// release should carry — and fetches the 0.3.0 zip in front of the `#`.
/// [`super::download::allowed`] already splits the host on `?` and `#` for the
/// same reason; this is the other place a URL stops meaning what it reads as.
fn from_our_releases(url: &str, file: &str) -> bool {
    let Some(rest) = url.strip_prefix(RELEASE_DOWNLOAD_PREFIX) else { return false };
    // Asked of the whole URL rather than of `rest`: the prefix contains none of
    // these today, and if it ever did the refusal should be total and obvious
    // rather than quietly confined to the part after it.
    !url.contains("..")
        && !url.contains('%')
        && !url.contains('\\')
        && !url.contains('?')
        && !url.contains('#')
        && rest.rsplit('/').next() == Some(file)
}

/// `Ok(None)` means "nothing to offer" and is not a problem: it covers the
/// common case (already current) and the awkward one (a release whose assets
/// are still uploading). `Err` is reserved for a body that could not be
/// understood at all, and reaches the user only as `update_check`'s returned
/// `Err` and the `Failed` event emitted beside it — this crate has no logger
/// and no console, so there is nowhere else for it to go.
pub fn newer_release(body: &str, current: &str) -> Result<Option<Release>, String> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| format!("release feed was not JSON: {e}"))?;

    let tag = json.get("tag_name").and_then(Value::as_str).ok_or("release feed has no tag_name")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);

    if !is_newer(version, current)? {
        return Ok(None);
    }

    let assets = json.get("assets").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let zip_name = zip_name(version);

    // Both assets or neither. A zip with no digest beside it cannot be
    // verified, and an unverifiable download is not an update -- it is a
    // failure waiting to happen halfway through the swap.
    let Some(zip) = find_asset(assets, &zip_name) else { return Ok(None) };
    let Some(sums) = find_asset(assets, SUMS_NAME) else { return Ok(None) };

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

    /// A URL under this repository's release downloads for `version`.
    fn ours(version: &str, file: &str) -> String {
        format!("https://github.com/tnhnblgl/trix/releases/download/v{version}/{file}")
    }

    /// A wholly genuine release of `version` — real URLs, real asset names —
    /// so a test that changes one thing about it is testing that one thing.
    fn genuine(version: &str) -> Release {
        Release {
            version: version.to_string(),
            notes_url: format!("https://github.com/tnhnblgl/trix/releases/tag/v{version}"),
            zip_url: ours(version, &zip_name(version)),
            sums_url: ours(version, SUMS_NAME),
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
        assert_eq!(
            release
                .asset_to_install("0.4.0")
                .expect("a release built from this repository's feed must pass"),
            "trix-v0.5.0-win-x64.zip",
            "and it must hand back the filename the download, the checksum and the URL all key off"
        );
    }

    /// The one field the webview supplies that reaches the filesystem.
    /// `update_install` interpolates it into the asset filename and joins that
    /// onto the staging directory, and `download::fetch_to_file` creates the
    /// parent before it writes — so a separator here does not fail, it writes
    /// somewhere else.
    #[test]
    fn a_version_that_could_name_something_other_than_a_file_is_refused() {
        for version in
            [r"..\..\..\Windows\System32\evil", "0.5.0/../../evil", r"0.5.0\evil", "C:/evil", ""]
        {
            let mut release = genuine("0.5.0");
            release.version = version.to_string();
            let Err(error) = release.asset_to_install("0.4.0") else {
                panic!("{version:?} must never become part of a path");
            };
            assert!(
                error.contains("not a version number"),
                "the refusal must be about the version, not about whatever failed later: {error}"
            );
        }
    }

    /// `newer_release` refuses to *offer* a downgrade, and until this check
    /// existed that was the only place the question was asked. `update_install`
    /// takes its release from the webview, so naming a genuine older release
    /// passed the origin check, the checksum and the version probe — every one
    /// of them, honestly — and installed an older Trix over a newer one.
    #[test]
    fn a_genuine_older_release_is_still_refused_at_install_time() {
        for version in ["0.3.0", "0.4.0"] {
            let Err(error) = genuine(version).asset_to_install("0.4.0") else {
                panic!("{version} is not newer than the running 0.4.0 and must not install");
            };
            assert!(
                error.contains("not newer"),
                "the user has to be told why an otherwise valid release was refused: {error}"
            );
        }
        genuine("0.5.0").asset_to_install("0.4.0").expect("and a real upgrade must still pass");
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
                .asset_to_install("0.4.0")
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
            // The URL's own last segment, so every case here is refused by the
            // rule it was written for rather than incidentally by the
            // filename rule -- which the three tests below cover on their own.
            let file = url.rsplit('/').next().expect("a URL has a last segment");
            assert!(!from_our_releases(url, file), "{url} must not read as a Trix release");
        }
    }

    /// The spelling `!url.contains("..")` cannot see. There is no dot in it at
    /// all, it starts with the prefix, and `download::allowed` passes it
    /// because the host really is `github.com` — so if the origin decodes
    /// before it normalises, this fetches an attacker's release, verifies it
    /// against the `SHA256SUMS.txt` they supplied beside it, and runs the
    /// `trix.exe` inside. Blacklisting spellings is what this test exists to
    /// stop anyone going back to.
    #[test]
    fn a_percent_encoded_traversal_is_refused() {
        const ENCODED: &str = "https://github.com/tnhnblgl/trix/releases/download/%2e%2e/%2e%2e/attacker/trix/releases/download/v0.5.0/trix-v0.5.0-win-x64.zip";
        assert!(
            !ENCODED.contains(".."),
            "the premise: the literal `..` rule sees nothing wrong with this URL"
        );
        assert!(ENCODED.starts_with(RELEASE_DOWNLOAD_PREFIX), "and the prefix rule passes it");

        let error = release_with(ENCODED, &ours("0.5.0", SUMS_NAME))
            .asset_to_install("0.4.0")
            .expect_err("a percent-encoded traversal must not read as a Trix release");
        assert!(error.contains(ENCODED), "the message must name the URL it refused: {error}");
    }

    /// A backslash is not a URL path character, and nothing `ship-zip.ps1`
    /// publishes contains one — but it is a path separator to some
    /// normalisers and not to others, which is the whole reason a URL would
    /// carry one. The case below is chosen so that the backslash rule is the
    /// only thing that can refuse it: the traversal spellings are already
    /// caught by `..`, and the filename at the end is the right one.
    #[test]
    fn a_url_containing_a_backslash_is_refused() {
        const BACKSLASH: &str =
            r"https://github.com/tnhnblgl/trix/releases/download/v0.5.0\x/trix-v0.5.0-win-x64.zip";
        assert!(!BACKSLASH.contains("..") && !BACKSLASH.contains('%'), "the premise");
        assert!(!from_our_releases(BACKSLASH, "trix-v0.5.0-win-x64.zip"));
    }

    /// The final-segment rule: a URL that resolves somewhere else has to name
    /// something else at the end of it — as long as the end of the string is
    /// also the end of what gets fetched, which is what the `?` and `#` rules
    /// are for and what the test below pins. It also closes an ordinary
    /// mix-up — a zip URL for a different version downloads one build, saves
    /// it under the name of another, and fails at the checksum with a message
    /// about GitHub.
    #[test]
    fn a_url_whose_last_segment_is_not_the_file_it_should_be_is_refused() {
        let wrong_zip =
            release_with(&ours("0.5.0", "trix-v0.4.0-win-x64.zip"), &ours("0.5.0", SUMS_NAME));
        let error = wrong_zip.asset_to_install("0.4.0").expect_err("a zip URL naming another file");
        assert!(error.contains("trix-v0.4.0-win-x64.zip"), "{error}");

        // The checksums half, which is the worse one to get wrong: the sums
        // file is what the zip is measured against.
        let wrong_sums = release_with(
            &ours("0.5.0", "trix-v0.5.0-win-x64.zip"),
            &ours("0.5.0", "not-the-checksums.txt"),
        );
        let error =
            wrong_sums.asset_to_install("0.4.0").expect_err("a sums URL naming another file");
        assert!(error.contains("not-the-checksums.txt"), "{error}");
    }

    /// Where the final-segment rule stops being able to speak for itself. A
    /// query string and a fragment both end the part of a URL that is fetched
    /// without ending the string, so the last segment names one asset while
    /// the request resolves to another — and every other rule here passes,
    /// which is exactly what makes this the shape worth pinning. Both URLs
    /// below are genuine releases of this repository up to the separator.
    #[test]
    fn a_url_that_names_one_asset_and_fetches_another_is_refused() {
        const NAMED: &str = "trix-v0.5.0-win-x64.zip";
        for url in [
            "https://github.com/tnhnblgl/trix/releases/download/v0.3.0/trix-v0.3.0-win-x64.zip#/trix-v0.5.0-win-x64.zip",
            "https://github.com/tnhnblgl/trix/releases/download/v0.3.0/trix-v0.3.0-win-x64.zip?x=/trix-v0.5.0-win-x64.zip",
        ] {
            assert!(
                !url.contains("..") && !url.contains('%') && !url.contains('\\'),
                "the premise: none of the other rules see anything wrong with {url}"
            );
            assert!(url.starts_with(RELEASE_DOWNLOAD_PREFIX), "and the prefix rule passes it");
            assert_eq!(
                url.rsplit('/').next(),
                Some(NAMED),
                "and it ends in the name a 0.5.0 release should carry, while what would be \
                 fetched is the 0.3.0 zip in front of the separator"
            );
            assert!(!from_our_releases(url, NAMED), "{url} must not read as a Trix release");
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

        release_with(OURS, OUR_SUMS).asset_to_install("0.4.0").expect("both from this repository");
        let error = release_with(OURS, THEIRS)
            .asset_to_install("0.4.0")
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
