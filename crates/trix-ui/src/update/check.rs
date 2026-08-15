//! Asking GitHub whether there is a newer Trix, and deciding whether the
//! answer is usable.
//!
//! Parsing is a free function over a `&str` so every rule below is testable
//! without a network, a server, or a fixture file. The one function that
//! touches the network does nothing but fetch the bytes.

use serde_json::Value;

/// `/releases/latest` rather than `/releases`: GitHub already excludes drafts
/// and prereleases from it, so a prerelease cannot reach users by accident.
pub const RELEASE_API: &str = "https://api.github.com/repos/tnhnblgl/trix/releases/latest";

/// GitHub rejects API requests that send no User-Agent.
pub fn user_agent() -> String {
    format!("trix/{}", env!("CARGO_PKG_VERSION"))
}

/// A release that is newer than what is running, and complete enough to install.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Release {
    pub version: String,
    pub notes_url: String,
    pub zip_url: String,
    pub sums_url: String,
    pub size: u64,
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

    /// GitHub rejects API requests with no User-Agent.
    #[test]
    fn the_user_agent_names_the_product_and_version() {
        let ua = user_agent();
        assert!(ua.starts_with("trix/"), "GitHub refuses requests without a User-Agent: {ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
    }
}
