// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The fake update of `SHIPO_FAKE_UPDATE`, for a [Feed::fakeable] feed: a check, a download and
//! an install that take about as long as real ones and fail as asked, with no network and no disk,
//! so an app's updater UI shows with no feed or release.

use std::path::PathBuf;
use std::time::Duration;

use crate::{Error, Feed, Progress, Release, Result, Staged, feed};

// MARK: Fake

/// `<version>` offers it, `none` nothing; `:fail-check` or `:fail-download` after either fails
/// that step.
pub const VAR: &str = "SHIPO_FAKE_UPDATE";

/// How long the check and the install take; the download, five times it. None in tests.
const PACE: Duration = Duration::from_secs(if cfg!(test) { 0 } else { 1 });

/// The step `:fail-<step>` fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
  Check,
  Download,
}

/// [VAR]'s value, read: the version offered (`None` for `none`) and the step that fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fake {
  version: Option<String>,
  fail: Option<Step>,
}

/// [VAR]'s value; an empty one is none.
pub fn var() -> Option<String> {
  std::env::var(VAR).ok().filter(|v| !v.is_empty())
}

/// [VAR]'s `value`, [Error::Feed] for one malformed.
fn parse(value: &str) -> Result<Fake> {
  let (version, fail) = match value.rsplit_once(':') {
    Some((v, "fail-check")) => (v, Some(Step::Check)),
    Some((v, "fail-download")) => (v, Some(Step::Download)),
    _ => (value, None),
  };

  let valid = !version.contains(|c: char| c.is_whitespace() || c == ':')
    && feed::newer(version, "0").is_some();
  match version {
    "none" => Ok(Fake { version: None, fail }),
    v if valid => Ok(Fake { version: Some(v.into()), fail }),
    _ => Err(Error::Feed(format!(
      "{VAR}={value:?} is not <version> or none, either with :fail-check or :fail-download"
    ))),
  }
}

/// [crate::check] faked by [VAR]'s `value`: the release as a feed of `feed`'s place would give it,
/// its notes and file under the GitHub repository's releases (example.com's for another host).
pub fn check(feed: &Feed, value: &str) -> Result<Option<Release>> {
  let fake = parse(value)?;
  std::thread::sleep(PACE);
  if fake.fail == Some(Step::Check) {
    return Err(Error::Feed(format!("{VAR}={value}: a fake failure, nothing was fetched")));
  }

  let Some(version) = fake.version.clone() else { return Ok(None) };
  let at = feed.url.find("/releases/").map(|i| &feed.url[..i + "/releases".len()]);
  let releases = at.unwrap_or("https://example.com/releases");
  let tag = format!("v{}", version.trim_start_matches('v'));
  let (notes, url) = (format!("{releases}/tag/{tag}"), format!("{releases}/download/{tag}/fake"));
  let (size, sha512, fake) = (50_000_000, [0; 64], Some(fake));
  Ok(Some(Release { version, notes: Some(notes), url, size, sha512, fake }))
}

/// [crate::download] of a fake release of `size` bytes: its progress told at the usual rate to the
/// end in five [PACE]s, or a failure midway; nothing fetched or written.
pub fn download(fake: &Fake, size: u64, progress: Progress) -> Result<Staged> {
  let steps: u64 = 20;
  for step in 1..=steps {
    std::thread::sleep(PACE * 5 / steps as u32);
    if fake.fail == Some(Step::Download) && step > steps * 2 / 5 {
      return Err(Error::Mismatch(format!("{VAR}: a fake failure, nothing was fetched")));
    }

    // In u128: any size an app sets, up to u64::MAX.
    progress((u128::from(size) * u128::from(step) / u128::from(steps)) as u64, size);
  }

  Ok(Staged { new: PathBuf::new(), target: PathBuf::new(), dir: None })
}

/// [crate::install] of a fake update: [Error::Unsupported] after a [PACE], nothing installed.
pub fn install() -> Result<()> {
  std::thread::sleep(PACE);
  Err(Error::Unsupported(format!("{VAR}: a fake update, nothing was installed")))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_value_is_a_version_or_none_and_a_step_to_fail() {
    let fake = |v: Option<&str>, fail| Fake { version: v.map(String::from), fail };
    let cases = [
      ("1.2.0", fake(Some("1.2.0"), None)),
      ("v2", fake(Some("v2"), None)),
      ("1.3.0-rc.1:fail-download", fake(Some("1.3.0-rc.1"), Some(Step::Download))),
      ("none", fake(None, None)),
      ("none:fail-check", fake(None, Some(Step::Check))),
    ];
    for (value, want) in cases {
      assert_eq!(parse(value).unwrap(), want, "{value}");
    }

    for value in [
      "",
      "yes",
      "1.2.0:fail",
      "1.2.0:fail-install",
      ":fail-check",
      " 1.2",
      "1.2:",
      "1.2.0-rc.1:fail-downlaod",
    ] {
      let e = parse(value).unwrap_err();
      assert!(matches!(&e, Error::Feed(m) if m.contains(VAR)), "{value:?}: {e}");
    }
  }

  #[test]
  fn a_fake_release_is_under_the_feeds_releases() {
    let github = "https://github.com/me/app/releases/latest/download/update.txt";
    let feed = |url| Feed::new(url, &[]).fakeable();
    let r = check(&feed(github), "1.2.0").unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.size), ("1.2.0", 50_000_000));
    let notes = r.notes.as_deref();
    assert_eq!(notes, Some("https://github.com/me/app/releases/tag/v1.2.0"));
    assert_eq!(r.url, "https://github.com/me/app/releases/download/v1.2.0/fake");
    let other = check(&feed("https://example.org/update.txt"), "v2").unwrap().unwrap();
    assert_eq!(other.notes.as_deref(), Some("https://example.com/releases/tag/v2"));
    assert_eq!(check(&feed(github), "none").unwrap(), None);
    let e = check(&feed(github), "1.2.0:fail-check").unwrap_err();
    assert!(matches!(&e, Error::Feed(m) if m.contains("fake")), "{e}");
  }
}
