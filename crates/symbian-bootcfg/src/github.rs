//! Turning a GitHub release into rows a phone can offer.
//!
//! `GET /repos/{owner}/{repo}/releases/latest` → JSON → [`crate::catalog::CatEntry`]s. That is the
//! whole provider, and it is small because the endpoint does most of the work: **`/releases/latest`
//! already excludes drafts and prereleases** by GitHub's own definition, so none of the filtering a
//! general client needs is needed here.
//!
//! ## What was taken from Obtainium, and what was not
//!
//! `ImranR98/Obtainium`'s `lib/app_sources/github.dart` is the reference for this problem, and it was
//! read before this was written. What it taught, and what this does with it:
//!
//! | Obtainium | Here |
//! |---|---|
//! | Rate limit is a first-class error: `x-ratelimit-remaining == '0'`, and it reports the minutes to reset | Same idea, different instrument — the shim exposes no arbitrary response headers, so [`RepoError::RateLimited`] is read off a **403 with a rate-limit body**. Unauthenticated GitHub allows 60 requests an hour per address, so this is a failure we *will* meet |
//! | Detects a renamed repository by refusing to follow redirects | `Fetch` follows them, but reports `was_redirected()` and `effective_url()`. Same warning, no new plumbing |
//! | Version from `tag_name`, optionally from the release title | `tag_name` only, through `Version::parse`, which now accepts the `v0.2.0` a git tag actually carries |
//! | Asset chosen by a user regex with an invert toggle | No regex engine on this phone. [`pkg::looks_like_package`] for the extension, plus an optional plain substring filter per repository — which covers "this release has `launcher.sisx` and `cal.sisx`" |
//! | Downloads from `browser_download_url` | The same |
//! | Configurable API host, for mirrors | The base lives on the [`crate::repo::Repo`], so a mirror needs no code |
//! | Five release sort modes, `build.gradle` appId inference, `/tags` fallback, `trackOnly`, search | **Not copied.** Only the latest release is wanted, and identity comes from inside the `.sis` rather than from anything the service says |
//!
//! The last row is the important one. Obtainium has to *infer* an app's identity because an APK's
//! package name is the best it can get from a listing. We do better and already do: the UID3 is read
//! out of the downloaded file by [`crate::sis::parse`], so nothing here has to be trusted about
//! identity — only about where to find bytes.

use alloc::string::String;
use alloc::vec::Vec;

use symbian_json::Json;

use crate::catalog::CatEntry;
use crate::pkg::{self, Version};

/// The default API base. Held as a constant rather than inlined so a mirror can replace it — the
/// thing Obtainium calls `GHReqPrefix`, and the reason a proxy would need no code.
pub const API_BASE: &str = "https://api.github.com";

/// The largest release payload this will accept, decoded.
///
/// Measured rather than guessed, on this handset: `rust-lang/rust` is 10012 bytes and 80% of that is
/// release notes; `BurntSushi/ripgrep`, with 28 assets, is 51036. 256 KB is an order of magnitude
/// above the worst real payload seen and far below anything that troubles a 4 MB heap.
pub const MAX_PAYLOAD: usize = 256 * 1024;

/// Why a check did not produce a catalogue. Shown to a person, so each one has to be actionable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RepoError {
    /// `owner/repo` was not two non-empty parts.
    BadTarget,
    /// The payload was not JSON. Carries the byte offset, because "bad JSON" is not actionable.
    BadJson(usize),
    /// 404. The repository is private, renamed, or has no published release — all three look the
    ///      same from here, and saying so is better than picking one.
    NotFound,
    /// 403 with a rate-limit body. Unauthenticated GitHub allows 60 requests an hour per address.
    RateLimited,
    /// Any other HTTP status.
    Status(u16),
    /// The release parsed and carried no `tag_name` that reads as a version.
    NoVersion,
    /// The release parsed and had no asset this phone can install.
    NoPackages,
}

impl RepoError {
    /// The sentence a person reads. Deliberately not the enum's `Debug`.
    pub fn describe(&self) -> &'static str {
        match self {
            RepoError::BadTarget => "not an owner/repo",
            RepoError::BadJson(_) => "the answer was not readable",
            RepoError::NotFound => "no release found (private, renamed, or none published)",
            RepoError::RateLimited => "GitHub's hourly limit; try again later",
            RepoError::Status(_) => "the server refused",
            RepoError::NoVersion => "the release has no version tag we can read",
            RepoError::NoPackages => "the release has no .sis to install",
        }
    }
}

/// Split `owner/repo`, tolerating what a person actually has in their clipboard.
///
/// A browser URL, a trailing slash, a `.git`, surrounding whitespace. Retyping a URL as `owner/repo`
/// is a step that exists only to be got wrong, and Obtainium takes the URL for the same reason.
pub fn split_target(raw: &str) -> Option<(String, String)> {
    let t = raw.trim();
    let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
    let t = t.strip_prefix("www.").unwrap_or(t);
    let t = t.strip_prefix("github.com/").unwrap_or(t);
    let t = t.trim_end_matches('/');
    let t = t.strip_suffix(".git").unwrap_or(t);
    let (owner, rest) = t.split_once('/')?;
    // Everything after the second slash is a path into the repository — `/releases`, `/tree/main` —
    // and not part of its name.
    let repo = rest.split('/').next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((String::from(owner), String::from(repo)))
}

/// The URL to ask.
///
/// `/releases/latest` and not `/releases`: only the latest is wanted, and the endpoint's own
/// semantics exclude drafts and prereleases so no filtering is needed. It is also the difference
/// between 10 KB and, at 30 releases a page, something around 300 KB.
pub fn latest_url(base: &str, owner: &str, repo: &str) -> String {
    alloc::format!("{}/repos/{owner}/{repo}/releases/latest", base.trim_end_matches('/'))
}

/// Read a status and a body into catalogue rows.
///
/// `filter`, when not empty, is a plain substring an asset's name must contain — the cheap stand-in
/// for Obtainium's regex, and enough for a release that ships several of our packages at once.
pub fn parse_release(
    repo_id: u16,
    status: u16,
    body: &[u8],
    filter: &str,
) -> Result<Vec<CatEntry>, RepoError> {
    match status {
        200 => {}
        404 => return Err(RepoError::NotFound),
        // A 403 here is nearly always the hourly limit rather than a permission: the body says so,
        // and the header that would say it more precisely is not one the shim can read.
        403 => {
            let text = String::from_utf8_lossy(body);
            return Err(if text.contains("rate limit") || text.contains("API rate") {
                RepoError::RateLimited
            } else {
                RepoError::Status(403)
            });
        }
        s => return Err(RepoError::Status(s)),
    }

    let doc = symbian_json::parse(body).map_err(|e| RepoError::BadJson(e.at))?;
    let tag = doc.get("tag_name").and_then(|t| t.as_str()).unwrap_or_default();
    let version = Version::parse(tag).ok_or(RepoError::NoVersion)?;
    // The repository's own name for itself, for grouping rows. `name` on a release is its title,
    // which is prose; the tag is not a name either. So: the asset's stem, decided per row below.
    // The notes ride along: they are in this payload already, and a second request to read what a
    // release says about itself would be a request per row.
    let notes = doc.get("body").and_then(|b| b.as_str()).unwrap_or_default();
    let out = collect_assets(repo_id, &doc, version, filter, notes);
    if out.is_empty() {
        return Err(RepoError::NoPackages);
    }
    Ok(out)
}

fn collect_assets(
    repo_id: u16,
    doc: &Json,
    version: Version,
    filter: &str,
    notes: &str,
) -> Vec<CatEntry> {
    let mut out = Vec::new();
    for a in doc.get("assets").map(|a| a.items()).unwrap_or_default() {
        let Some(name) = a.get("name").and_then(|n| n.as_str()) else { continue };
        // The extension filter first, because it is the one that is not a preference: this phone
        // installs `.sis` and `.sisx` and nothing else. Real releases carry `.sha256` sidecars and
        // tarballs for six architectures — 28 assets on the ripgrep release measured on this
        // handset — so without it every row would be noise.
        if !pkg::looks_like_package(name) {
            continue;
        }
        if !filter.is_empty() && !name.contains(filter) {
            continue;
        }
        let Some(url) = a.get("browser_download_url").and_then(|u| u.as_str()) else { continue };
        out.push(CatEntry {
            repo_id,
            asset: String::from(name),
            name: String::from(stem_of(name)),
            version,
            url: String::from(url),
            size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
            // The same notes on every asset of one release, because they describe the release and
            // not the file. Two packages published together share what was said about them.
            notes: String::from(notes),
        });
    }
    out
}

/// `launcher-0.2.0.sisx` → `launcher`. The label a row carries until the file itself can say what it
/// is; a version in the name is dropped because the version is its own column.
fn stem_of(asset: &str) -> &str {
    let base = asset.rsplit_once('.').map(|(a, _)| a).unwrap_or(asset);
    match base.split_once('-') {
        // Only when what follows looks like a version, so `boot-manager.sisx` keeps its name.
        Some((head, tail)) if Version::parse(tail).is_some() && !head.is_empty() => head,
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release `BurntSushi/ripgrep` was serving when this handset fetched it: 51036 bytes, 28
    /// assets, `.sha256` sidecars and tarballs for six architectures. Real, and pulled off the phone
    /// — a fixture written here would only prove that it agrees with this file.
    const REAL_ASSETS: &[u8] = include_bytes!("../../symbian-json/tests/release_assets.json");
    /// `rust-lang/rust` 1.98.0: 10012 bytes, no assets at all, and four fifths release notes.
    const REAL_NOTES: &[u8] = include_bytes!("../../symbian-json/tests/ghprobe.json");

    #[test]
    fn a_target_survives_whatever_is_in_the_clipboard() {
        let want = (String::from("pizzaria-foundation"), String::from("home"));
        for raw in [
            "pizzaria-foundation/home",
            "  pizzaria-foundation/home  ",
            "https://github.com/pizzaria-foundation/home",
            "http://www.github.com/pizzaria-foundation/home/",
            "github.com/pizzaria-foundation/home.git",
            "https://github.com/pizzaria-foundation/home/releases/tag/v0.2.0",
        ] {
            assert_eq!(split_target(raw), Some(want.clone()), "{raw}");
        }
        assert_eq!(split_target("nothing"), None);
        assert_eq!(split_target("/home"), None);
        assert_eq!(split_target("owner/"), None);
        assert_eq!(split_target(""), None);
    }

    #[test]
    fn the_url_asked_is_the_latest_release() {
        assert_eq!(
            latest_url(API_BASE, "a", "b"),
            "https://api.github.com/repos/a/b/releases/latest"
        );
        // A mirror needs no code, only a different base — Obtainium's `GHReqPrefix` in one argument.
        assert_eq!(latest_url("https://gh.example/api/", "a", "b"),
            "https://gh.example/api/repos/a/b/releases/latest");
    }

    #[test]
    fn a_real_release_with_no_sis_says_so_rather_than_offering_nothing_quietly() {
        // 28 assets and not one this phone can install. "No .sis to install" is a sentence somebody
        // can act on; an empty list is not.
        let e = parse_release(1, 200, REAL_ASSETS, "").unwrap_err();
        assert_eq!(e, RepoError::NoPackages);
        assert_eq!(e.describe(), "the release has no .sis to install");
    }

    #[test]
    fn a_real_release_with_no_assets_at_all_is_the_same_answer() {
        assert_eq!(parse_release(1, 200, REAL_NOTES, "").unwrap_err(), RepoError::NoPackages);
    }

    /// The ripgrep payload with its asset names rewritten to ours, so the extraction is exercised
    /// against a document with the real shape — 28 entries, sidecars, the same nesting — rather than
    /// against three objects somebody typed.
    fn ours() -> Vec<u8> {
        let text = String::from_utf8_lossy(REAL_ASSETS).into_owned();
        text.replace("ripgrep-15.2.0-aarch64-apple-darwin.tar.gz.sha256", "launcher-0.2.0.sisx")
            .replace("ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz", "cal.sis")
            .into_bytes()
    }

    #[test]
    fn our_packages_are_picked_out_of_a_real_release() {
        let rows = parse_release(7, 200, &ours(), "").expect("two of ours in there");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.repo_id == 7));
        assert!(rows.iter().all(|r| r.version == Version::new(15, 2, 0)), "from the tag");

        let launcher = rows.iter().find(|r| r.asset == "launcher-0.2.0.sisx").unwrap();
        assert_eq!(launcher.name, "launcher", "the version comes out of the label");
        assert!(launcher.url.starts_with("https://github.com/"));
        let cal = rows.iter().find(|r| r.asset == "cal.sis").unwrap();
        assert_eq!(cal.name, "cal");
        assert!(cal.size > 0, "the size is the progress bar's denominator");
    }

    #[test]
    fn a_substring_filter_narrows_a_release_that_ships_several_of_ours() {
        let rows = parse_release(7, 200, &ours(), "launcher").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].asset, "launcher-0.2.0.sisx");
        // And a filter nothing matches is the same honest answer as a release with nothing in it.
        assert_eq!(parse_release(7, 200, &ours(), "nope").unwrap_err(), RepoError::NoPackages);
    }

    #[test]
    fn every_http_answer_becomes_something_a_person_can_act_on() {
        assert_eq!(parse_release(1, 404, b"", "").unwrap_err(), RepoError::NotFound);
        // The hourly limit is the failure this will actually meet: 60 requests an hour, no token.
        let limited = br#"{"message":"API rate limit exceeded for 1.2.3.4."}"#;
        assert_eq!(parse_release(1, 403, limited, "").unwrap_err(), RepoError::RateLimited);
        // A 403 that is not the limit stays a 403 rather than being explained away.
        assert_eq!(
            parse_release(1, 403, br#"{"message":"Must have push access"}"#, "").unwrap_err(),
            RepoError::Status(403)
        );
        assert_eq!(parse_release(1, 500, b"", "").unwrap_err(), RepoError::Status(500));
    }

    #[test]
    fn rubbish_reports_where_it_stopped() {
        // "The JSON was bad" is not actionable; a byte offset is.
        match parse_release(1, 200, b"{\"tag_name\":", "").unwrap_err() {
            RepoError::BadJson(at) => assert!(at > 0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_release_with_no_readable_tag_is_refused_rather_than_dated_zero() {
        let doc = br#"{"tag_name":"nightly","assets":[]}"#;
        assert_eq!(parse_release(1, 200, doc, "").unwrap_err(), RepoError::NoVersion);
    }

    #[test]
    fn a_tag_with_the_v_a_git_tag_actually_carries() {
        let doc = br#"{"tag_name":"v0.2.0","assets":[{"name":"launcher.sisx","size":10,
            "browser_download_url":"https://x/launcher.sisx"}]}"#;
        let rows = parse_release(1, 200, doc, "").unwrap();
        assert_eq!(rows[0].version, Version::new(0, 2, 0));
    }

    #[test]
    fn an_asset_missing_its_url_is_skipped_and_does_not_sink_the_rest() {
        let doc = br#"{"tag_name":"1.0.0","assets":[
            {"name":"broken.sisx","size":1},
            {"name":"good.sisx","size":2,"browser_download_url":"https://x/good.sisx"}]}"#;
        let rows = parse_release(1, 200, doc, "").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].asset, "good.sisx");
    }

    #[test]
    fn a_label_keeps_a_hyphen_that_is_not_a_version() {
        assert_eq!(stem_of("boot-manager.sisx"), "boot-manager");
        assert_eq!(stem_of("launcher-0.2.0.sisx"), "launcher");
        assert_eq!(stem_of("launcher.sis"), "launcher");
        assert_eq!(stem_of("noext"), "noext");
    }
}
