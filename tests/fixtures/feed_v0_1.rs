// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! v0.1.0's feed reader, frozen for the packer's tests: what every app installed with v0.1 reads.
//! Its `src/feed.rs` at the v0.1.0 tag, the reading alone: never change it.

use ed25519_compact::{PublicKey, Signature};

/// v0.1.0's `Error`, the variants its reader gives.
#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
  Feed(String),
  Signature(String),
  Keys,
}

type Result<T> = std::result::Result<T, Error>;

/// The release a v0.1 app on `platform` at version `current` takes from the feed `text` signed by
/// any of `keys`: v0.1.0's `Feed::read` and `public_keys`.
pub fn read(keys: &[&str], text: &[u8], current: &str, platform: &str) -> Result<Option<Release>> {
  let (data, sig) = signed(text)?;
  let key = |k: &str| hex(k).map(PublicKey::new).filter(|k| k.validate().is_ok());
  let keys = keys.iter().map(|k| key(k)).collect::<Option<Vec<_>>>();
  let keys = keys.filter(|k| !k.is_empty()).ok_or(Error::Keys)?;
  verify(&keys, data.as_bytes(), sig)?;
  release(data, current, platform)
}

/// v0.1.0's `Release`.
#[derive(Debug, PartialEq, Eq)]
pub struct Release {
  pub version: String,
  pub notes: Option<String>,
  pub url: String,
  pub size: u64,
  pub sha512: [u8; 64],
}

// MARK: Signature

/// The feed `text` split at its last line, `sig <signature>`: the text before that line and the
/// signature's hex; [Error::Feed] if it is not UTF-8, [Error::Signature] if the last line is not
/// that or another line is too.
fn signed(text: &[u8]) -> Result<(&str, &str)> {
  let text = std::str::from_utf8(text).map_err(|_| Error::Feed("not UTF-8".into()))?;
  let end = text.strip_suffix('\n').unwrap_or(text);
  let start = end.rfind('\n').map_or(0, |i| i + 1);
  let (data, last) = (&text[..start], &end[start..]);
  let sig = last.strip_suffix('\r').unwrap_or(last).strip_prefix("sig ");
  match sig.filter(|_| !data.lines().any(|l| l.split(' ').next() == Some("sig"))) {
    Some(sig) => Ok((data, sig)),
    None => Err(Error::Signature("the last line is not the feed's one `sig` line".into())),
  }
}

/// Checks `sig`, the hex Ed25519 signature of `data`, by any of `keys`.
fn verify(keys: &[PublicKey], data: &[u8], sig: &str) -> Result<()> {
  let sig = hex(sig).map(Signature::new);
  match sig.is_some_and(|s| keys.iter().any(|k| k.verify(data, &s).is_ok())) {
    true => Ok(()),
    false => Err(Error::Signature("no key of the app verifies it".into())),
  }
}

// MARK: Parsing

/// The release in the feed `text` for `platform` when its version is newer than `current`: one
/// `version` of one word, at most one `notes` of one [secure] URL and one line of the platform's
/// `<size> <sha512> <url>`, all checked before the versions are; a line of another first word
/// skipped, repeated or not.
pub fn release(text: &str, current: &str, platform: &str) -> Result<Option<Release>> {
  let bad = |what: String| Err(Error::Feed(what));
  let (mut version, mut notes, mut file) = (None, None, None);
  for line in text.lines() {
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let slot = match word {
      "" => return bad(format!("line {line:?} has no first word")),
      "version" => &mut version,
      "notes" => &mut notes,
      _ if word == platform => &mut file,
      // A record of a later format, or of another platform.
      _ => continue,
    };

    if slot.replace(rest).is_some() {
      return bad(format!("its {word} line is repeated"));
    }
  }

  let version = version.filter(|v| !v.is_empty() && !v.contains(' '));
  let Some(version) = version else { return bad("no version, or a bad one".into()) };
  if let Some(n) = notes.filter(|n| !secure(n)) {
    return bad(format!("notes {n:?} are not one https URL"));
  }

  let file = file.map(|f| fields(f, platform)).transpose()?;
  match newer(version, current) {
    None => return bad(format!("versions {version} and {current} don't compare")),
    Some(false) => return Ok(None),
    Some(true) => {}
  }

  let Some((size, sha512, url)) = file else { return Ok(None) };
  let (version, notes, url) = (version.into(), notes.map(String::from), url.into());
  Ok(Some(Release { version, notes, url, size, sha512 }))
}

/// A platform's `<size> <sha512> <url>`, of its line's `rest`.
fn fields<'a>(rest: &'a str, platform: &str) -> Result<(u64, [u8; 64], &'a str)> {
  let what = || Error::Feed(format!("its {platform} line is not <size> <sha512> <url>"));
  let [size, sha512, url] = rest.split(' ').collect::<Vec<_>>()[..] else { return Err(what()) };
  let size = size.parse().ok().filter(|_| size.bytes().all(|b| b.is_ascii_digit()));
  let (Some(size), Some(sha512)) = (size, hex(sha512)) else { return Err(what()) };
  match secure(url) {
    true => Ok((size, sha512, url)),
    false => Err(Error::Feed(format!("{url:?} is not https"))),
  }
}

/// `url` is https to a host, or http on [loopback] (tests, a VM serving from its own host), all
/// printable ASCII: no space or control character a link could hide.
pub fn secure(url: &str) -> bool {
  let to = authority(url, "https://").is_some_and(|h| !h.is_empty()) || loopback(url);
  to && url.bytes().all(|b| b.is_ascii_graphic())
}

/// `url` is http to this machine: `127.0.0.1`, `localhost` or `[::1]`.
pub fn loopback(url: &str) -> bool {
  let Some(host) = authority(url, "http://") else { return false };
  let host = match host.rsplit_once(':') {
    Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
    _ => host,
  };

  ["127.0.0.1", "localhost", "[::1]"].contains(&host)
}

/// The host (and port) of `url` when it starts with `scheme`: what comes before its path, query
/// or fragment.
fn authority<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
  url.strip_prefix(scheme)?.split(['/', '?', '#']).next()
}

/// `N` bytes from exactly `2 * N` hex digits, of either case.
pub fn hex<const N: usize>(s: &str) -> Option<[u8; N]> {
  if s.len() != 2 * N || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
    return None;
  }

  let bytes = (0..N).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok());
  bytes.collect::<Option<Vec<u8>>>()?.try_into().ok()
}

// MARK: Versions

/// A pre-release's dot part: numbers below words, as semver orders them.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Part<'a> {
  Num(u64),
  Word(&'a str),
}

/// Whether version `v` comes after `than`: numeric dot parts (`1.10` after `1.9`, `1.2` equal to
/// `1.2.0`), then a release after its pre-releases (`1.2.0-dev`); a leading `v` and `+build`
/// are ignored. `None` if one isn't a version.
fn newer(v: &str, than: &str) -> Option<bool> {
  fn key(v: &str) -> Option<(Vec<u64>, bool, Vec<Part<'_>>)> {
    let v = v.trim().trim_start_matches('v').split('+').next()?;
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let mut nums = core.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
    while nums.last() == Some(&0) {
      nums.pop();
    }

    let parts = pre.unwrap_or_default().split('.');
    Some((nums, pre.is_none(), parts.map(|p| p.parse().map_or(Part::Word(p), Part::Num)).collect()))
  }

  Some(key(v)? > key(than)?)
}
