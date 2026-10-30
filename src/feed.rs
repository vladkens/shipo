// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The feed and the release it offers, read as `spec.md` gives it: its `sig` line checked by any
//! of the app's keys first, then the release for this platform when its version is newer than the
//! running one.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use ed25519_compact::sha512::Hash;
use ed25519_compact::{PublicKey, Signature};

use crate::{bad, error};

// MARK: Feed and release

/// Where an app's updates come from: its feed's URL and the Ed25519 public keys that may sign it,
/// a const in the app made by [Feed::new].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Feed<'a> {
  /// `update.txt`'s URL.
  pub url: &'a str,
  /// The public keys, each its 32 bytes in 64 hex digits: a feed signed by any of them is taken.
  /// `shipo keygen` makes two, the working one and the spare kept offline, so a leaked key can be
  /// dropped by a release the other signs (`spec.md`).
  pub keys: &'a [&'a str],
}

impl<'a> Feed<'a> {
  /// The feed at `url` signed by any of `keys`. An option added later comes as a method of its
  /// own, so it breaks no app's const.
  pub const fn new(url: &'a str, keys: &'a [&'a str]) -> Self {
    Self { url, keys }
  }

  /// The release the feed's bytes `text`, fetched by the app itself, offer this platform when it
  /// is newer than `current`; [crate::check] fetches and reads in one. Its `sig` line is checked
  /// first (`InvalidData` if it is not the last line, comes twice or no key verifies it;
  /// `InvalidInput` for no key or a malformed one), the feed read after; a version not newer, or
  /// none for this platform, is `None`.
  pub fn read(&self, text: &[u8], current: &str) -> io::Result<Option<Release>> {
    let (data, sig) = signed(text)?;
    verify(&self.public_keys()?, data.as_bytes(), sig)?;
    release(data, current, &platform())
  }

  /// The keys, each 64 hex digits of a valid Ed25519 public key; `InvalidInput`, whatever the
  /// feed, for none or a malformed one: the app's own mistake.
  pub(crate) fn public_keys(&self) -> io::Result<Vec<PublicKey>> {
    let key = |k: &str| hex(k).map(PublicKey::new).filter(|k| k.validate().is_ok());
    let keys = self.keys.iter().map(|k| key(k)).collect::<Option<Vec<_>>>();
    let keys = keys.filter(|k| !k.is_empty());
    keys.ok_or_else(|| error(io::ErrorKind::InvalidInput, "the app has no key, or a bad one"))
  }
}

/// A newer version the feed offers this platform, made by [crate::check] and [Feed::read] alone.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Release {
  pub version: String,
  /// The release's page, for its notes (`<repository>/releases/tag/<tag>`); `None` in a feed
  /// without one.
  pub notes: Option<String>,
  /// The file to download, https (or loopback).
  pub url: String,
  /// The file's size in bytes.
  pub size: u64,
  /// The file's SHA-512.
  pub sha512: [u8; 64],
}

impl Release {
  /// A release of `version` alone, for an app's own tests: no notes and no file (an empty URL,
  /// size 0, a zero hash), so [crate::download] refuses it.
  #[doc(hidden)]
  pub fn test(version: &str) -> Self {
    Self { version: version.into(), notes: None, url: String::new(), size: 0, sha512: [0; 64] }
  }

  /// Checks the file downloaded for the release against its size and SHA-512: `InvalidData` if
  /// either differs.
  pub(crate) fn verify(&self, file: &Path) -> io::Result<()> {
    match fs::metadata(file)?.len() == self.size && sha512(file)? == self.sha512 {
      true => Ok(()),
      false => Err(bad(format!("{} is not the file of {}", file.display(), self.version))),
    }
  }
}

/// The most bytes a feed may be.
pub const MAX: u64 = 1 << 20;

/// This platform as the feed names it, `<os>-<arch>` of [std::env::consts].
pub fn platform() -> String {
  format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

// MARK: Signature and hash

/// The feed `text` split at its last line, `sig <signature>`: the text before that line and the
/// signature's hex; `InvalidData` if it is not UTF-8, the last line is not that or another line is
/// too.
fn signed(text: &[u8]) -> io::Result<(&str, &str)> {
  let text = std::str::from_utf8(text).map_err(|_| bad("the feed is not UTF-8"))?;
  let end = text.strip_suffix('\n').unwrap_or(text);
  let start = end.rfind('\n').map_or(0, |i| i + 1);
  let (data, last) = (&text[..start], &end[start..]);
  let sig = last.strip_suffix('\r').unwrap_or(last).strip_prefix("sig ");
  match sig.filter(|_| !data.lines().any(|l| l.split(' ').next() == Some("sig"))) {
    Some(sig) => Ok((data, sig)),
    None => Err(bad("the feed's last line is not its one `sig` line")),
  }
}

/// Checks `sig`, the hex Ed25519 signature of `data`, by any of `keys`.
fn verify(keys: &[PublicKey], data: &[u8], sig: &str) -> io::Result<()> {
  let sig = hex(sig).map(Signature::new);
  match sig.is_some_and(|s| keys.iter().any(|k| k.verify(data, &s).is_ok())) {
    true => Ok(()),
    false => Err(bad("the feed's signature is wrong")),
  }
}

/// The SHA-512 of the file at `path`, read in chunks.
fn sha512(path: &Path) -> io::Result<[u8; 64]> {
  let (mut file, mut hash, mut buf) = (File::open(path)?, Hash::new(), vec![0; 1 << 16]);
  loop {
    match file.read(&mut buf) {
      Ok(0) => return Ok(hash.finalize()),
      Ok(n) => hash.update(&buf[..n]),
      Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
      Err(e) => return Err(e),
    }
  }
}

// MARK: Parsing

/// The release in the feed `text` for `platform` when its version is newer than `current`: one
/// `version` of one word, at most one `notes` of one [secure] URL and one line of the platform's
/// `<size> <sha512> <url>`, all checked before the versions are; a line of another first word
/// skipped, repeated or not.
pub fn release(text: &str, current: &str, platform: &str) -> io::Result<Option<Release>> {
  let (mut version, mut notes, mut file) = (None, None, None);
  for line in text.lines() {
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let slot = match word {
      "" => return Err(bad(format!("the feed's line {line:?} has no first word"))),
      "version" => &mut version,
      "notes" => &mut notes,
      _ if word == platform => &mut file,
      // A record of a later format, or of another platform.
      _ => continue,
    };

    if slot.replace(rest).is_some() {
      return Err(bad(format!("the feed's {word} line is repeated")));
    }
  }

  let version = version.filter(|v| !v.is_empty() && !v.contains(' '));
  let version = version.ok_or_else(|| bad("the feed has no version, or a bad one"))?;
  if let Some(n) = notes.filter(|n| !secure(n)) {
    return Err(bad(format!("the feed's notes {n:?} are not one https URL")));
  }

  let file = file.map(|f| fields(f, platform)).transpose()?;
  match newer(version, current) {
    None => return Err(bad(format!("versions {version} and {current} don't compare"))),
    Some(false) => return Ok(None),
    Some(true) => {}
  }

  let Some((size, sha512, url)) = file else { return Ok(None) };
  let (version, notes, url) = (version.into(), notes.map(String::from), url.into());
  Ok(Some(Release { version, notes, url, size, sha512 }))
}

/// A platform's `<size> <sha512> <url>`, of its line's `rest`.
fn fields<'a>(rest: &'a str, platform: &str) -> io::Result<(u64, [u8; 64], &'a str)> {
  let what = || bad(format!("the feed's {platform} line is not <size> <sha512> <url>"));
  let [size, sha512, url] = rest.split(' ').collect::<Vec<_>>()[..] else { return Err(what()) };
  let size = size.parse().ok().filter(|_| size.bytes().all(|b| b.is_ascii_digit()));
  let (Some(size), Some(sha512)) = (size, hex(sha512)) else { return Err(what()) };
  match secure(url) {
    true => Ok((size, sha512, url)),
    false => Err(bad(format!("{url} is not https"))),
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::testing::{ABC, FEED, KEY, RFC_KEY, dir, sign, write};

  /// [FEED]'s lines before `sig` ended in CRLF, signed so.
  const CRLF_SIG: &str = "894a66f5a20e1013bef7720bdcfd2ecad93f289bd483adaca9f7ead1a450201e23f4282df9f4cc03c361ddac43db127f8be20ba9bb2a32779d9ffb61adc2e008";
  /// RFC 8032's test 1: the signature of the empty message by [RFC_KEY].
  const RFC_SIG: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

  /// The keys of `hex`, which the tests give valid.
  fn keys(hex: &[&str]) -> Vec<PublicKey> {
    Feed::new("", hex).public_keys().unwrap()
  }

  #[test]
  fn a_feed_gives_its_release() {
    let r = release(FEED, "1.1.9", "macos-aarch64").unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.url.as_str()), ("1.2.0", "https://example.com/app.zip"));
    assert_eq!((r.size, r.sha512, r.notes), (3, hex(ABC).unwrap(), None));
    let crlf = format!("version 2\r\nnotes https://n\r\np 1 {ABC} https://a\r\n");
    let r = release(&crlf, "1", "p").unwrap().unwrap();
    assert_eq!((r.url.as_str(), r.notes.as_deref()), ("https://a", Some("https://n")), "CRLF");
  }

  #[test]
  fn the_notes_are_the_releases_page() {
    let page = "https://github.com/me/app/releases/tag/v2";
    let feed = |notes: &str| format!("version 2\nnotes {notes}\np 1 {ABC} https://a\n");
    let r = release(&feed(page), "1", "p").unwrap().unwrap();
    assert_eq!(r.notes.as_deref(), Some(page));
    for notes in ["", "http://example.com/v2", "file:///etc", "https://a b", "https://a\tb"] {
      let e = release(&feed(notes), "1", "p").unwrap_err();
      assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{notes:?}: {e}");
    }

    let twice = format!("{}notes {page}\n", feed(page));
    assert!(release(&twice, "1", "p").is_err(), "twice");
    let bad = feed("javascript:alert(1)");
    assert!(release(&bad, "2", "p").is_err(), "checked when up to date too");
    assert!(release(&bad, "1", "q").is_err(), "and with no line for the platform");
  }

  #[test]
  fn a_later_formats_records_and_blocks_are_skipped() {
    // A notes file per language, and a block of text whose lines would be records of their own.
    let (p, sig) = (platform(), sign(b""));
    let later = format!(
      "notes-text en 3 {ABC} https://a/en.md\nnotes-text ru 3 {ABC} https://a/ru.md\ntext en\n\
       | ## What's new\n|\n|   code\n| version 9\n| notes x\n| {p} 1 00 x\n| sig {sig}\n"
    );
    let body = format!("version 2\n{later}{p} 3 {ABC} https://a/f\n");
    let feed = format!("{body}sig {}\n", sign(body.as_bytes()));
    let r = Feed::new("https://a", &[RFC_KEY]).read(feed.as_bytes(), "1").unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.url.as_str(), r.notes), ("2", "https://a/f", None));
  }

  #[test]
  fn no_file_for_the_platform_is_no_update() {
    assert_eq!(release(FEED, "1.0", "plan9-mips").unwrap(), None);
  }

  #[test]
  fn a_malformed_feed_is_refused() {
    let line = |file: &str| format!("version 2\np {file}\n");
    let files =
      ["1 00 https://a", "-1 {ABC} https://a", "+1 {ABC} https://a", "1.5 {ABC} https://a"];
    let more = ["{ABC} https://a", "1 {ABC}", "1 {ABC} https://a x", "1  {ABC} https://a", ""];
    for file in files.into_iter().chain(more) {
      let feed = line(&file.replace("{ABC}", ABC));
      for current in ["1", "3"] {
        let e = release(&feed, current, "p").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{file} over {current}: {e}");
      }
    }

    let ok = line(&format!("1 {ABC} https://a"));
    for feed in [
      "".to_owned(),
      "p 1 x\n".to_owned(),
      "version\n".to_owned(),
      "version 2 3\n".to_owned(),
      format!("{ok}version 3\n"),
      format!("{ok}p 1 {ABC} https://b\n"),
      format!("{ok}\n"),
      format!(" {ok}"),
    ] {
      assert!(release(&feed, "1", "p").is_err(), "{feed:?}");
    }

    assert!(signed(b"version \xff\nsig 00\n").is_err(), "not UTF-8");
    let other = format!("{ok}q 1 x\nq 2 y\nwhatever comes later\nwhatever comes later\n");
    let r = release(&other, "1", "p").unwrap();
    assert!(r.is_some(), "other lines are skipped, repeated or not");
  }

  #[test]
  fn hex_of_either_case() {
    assert_eq!(hex("00fF7a"), Some([0, 255, 122]));
    for s in ["00ff", "00ff7a0", "00ff7a00", "+0ff7a", "00ffxx", "00ffé", ""] {
      assert_eq!(hex::<3>(s), None, "{s}");
    }
  }

  #[test]
  fn versions_order() {
    let newer = |a, b| newer(a, b).unwrap();
    assert!(newer("1.10", "1.9"));
    assert!(newer("1.2.0", "1.2.0-dev"));
    assert!(newer("v2.0", "1.99.99"));
    assert!(newer("1.2.0-rc.10", "1.2.0-rc.9"));
    assert!(newer("1.2.0-rc", "1.2.0-1"));
    assert!(!newer("1.2.0-dev", "1.2.0"));
    assert!(!newer("1.2", "1.2.0"), "equal is no update");
    assert!(!newer("1.2.0+5", "1.2.0"));
    assert!(!newer("1.1.9", "1.2.0"), "older is no update");
    assert_eq!(super::newer("1.x", "1.0"), None);
    assert_eq!(release(FEED, "1.2.0", "macos-aarch64").unwrap(), None);
    assert_eq!(release(FEED, "1.3", "macos-aarch64").unwrap(), None);
    assert!(release(FEED, "", "macos-aarch64").is_err());
  }

  #[test]
  fn https_to_a_host_and_http_only_for_loopback() {
    assert!(release(FEED, "1.0", "test-http").is_err());
    for url in ["http://127.0.0.1:8000/a.zip", "http://localhost/a", "http://[::1]:80", "https://a"]
    {
      assert!(secure(url), "{url}");
    }

    let hidden = ["https://a b", "https://a\tb", "https://a\rb", "https://a\x1b[2J", "https://é"];
    for url in ["http://example.com/a", "http://127.0.0.1.evil.com/", "http://127.0.0.1@evil.com/"]
      .into_iter()
      .chain(["https://", "https:///a", "https://?a", "ftp://localhost/a"])
      .chain(hidden)
    {
      assert!(!secure(url), "{url:?}");
    }
  }

  #[test]
  fn rfc_8032_test_1_verifies() {
    verify(&keys(&[RFC_KEY]), b"", RFC_SIG).unwrap();
    assert!(verify(&keys(&[RFC_KEY]), b"x", RFC_SIG).is_err());
    assert_eq!(sign(b""), RFC_SIG, "the tests' signer is RFC 8032's");
  }

  /// [FEED] read with [KEY].
  fn read_feed(text: &str) -> io::Result<Option<Release>> {
    Feed::new("https://example.com/update.txt", &[KEY]).read(text.as_bytes(), "1.0")
  }

  #[test]
  fn an_openssl_signed_feed_verifies() {
    let (data, sig) = signed(FEED.as_bytes()).unwrap();
    assert_eq!((data.len(), sig.len()), (FEED.len() - 133, 128));
    verify(&keys(&[KEY]), data.as_bytes(), sig).unwrap();
    // A release only where the feed has a line for this platform (not on ARM Windows or Linux).
    read_feed(FEED).unwrap();
    // The same lines ended in CRLF, the `sig` line too or not, or with no line end.
    let crlf = data.replace('\n', "\r\n");
    for end in ["\r\n", "\n", ""] {
      let feed = format!("{crlf}sig {CRLF_SIG}{end}");
      assert_eq!(read_feed(&feed).unwrap(), read_feed(FEED).unwrap(), "{end:?}");
    }
  }

  #[test]
  fn a_feed_signed_by_any_of_the_keys_verifies() {
    let (data, sig) = signed(FEED.as_bytes()).unwrap();
    for hex in [[KEY, RFC_KEY], [RFC_KEY, KEY]] {
      verify(&keys(&hex), data.as_bytes(), sig).unwrap();
    }

    // After a leak the spare signs: an app holding both keys takes it, one with the working key
    // alone doesn't.
    let body = format!("version 2\nnotes https://a/v2\n{} 3 {ABC} https://a/f\n", platform());
    let feed = format!("{body}sig {}\n", sign(body.as_bytes()));
    let both = Feed::new("https://a", &[KEY, RFC_KEY]);
    let r = both.read(feed.as_bytes(), "1").unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.notes.as_deref()), ("2", Some("https://a/v2")));
    let working = Feed::new("https://a", &[KEY]);
    let e = working.read(feed.as_bytes(), "1").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
  }

  #[test]
  fn the_apps_keys_are_valid_or_no_feed_is_read() {
    // Not hex, a valid point's hex too short, a small-order point (all zeros), none.
    let zero = "00".repeat(32);
    for hex in [&["AAAA"][..], &[KEY, &KEY[2..]], &[KEY, &zero], &[]] {
      let feed = Feed::new("https://a", hex);
      assert_eq!(
        feed.read(FEED.as_bytes(), "1.0").unwrap_err().kind(),
        io::ErrorKind::InvalidInput,
        "{hex:?}"
      );
    }
  }

  #[test]
  fn the_sig_line_is_last_and_once() {
    let (data, sig) = signed(FEED.as_bytes()).unwrap();
    for feed in [
      data.to_owned(),
      format!("{data}sig {sig}\n\n"),
      format!("{data}sig {sig}\nsig {sig}\n"),
      format!("sig {sig}\n{data}sig {sig}\n"),
      format!("{data}sig\nsig {sig}\n"),
      format!("{data}sig {sig}\nversion 1.3\n"),
      format!("{data}sig  {sig}\n"),
      format!("{data}sig {sig} \n"),
    ] {
      let e = read_feed(&feed).unwrap_err();
      assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{feed:?}: {e}");
    }
  }

  #[test]
  fn a_changed_feed_or_another_key_is_refused() {
    let (data, sig) = signed(FEED.as_bytes()).unwrap();
    // Each byte the signature covers: a line's text, its line end, the line before `sig`.
    for at in [0, 20, data.len() - 2, data.len() - 1] {
      let mut feed = FEED.as_bytes().to_vec();
      feed[at] ^= 1;
      let read = Feed::new("https://a", &[KEY]).read(&feed, "1.0");
      assert_eq!(read.unwrap_err().kind(), io::ErrorKind::InvalidData, "{at}");
    }

    let kind = |r: io::Result<()>| r.unwrap_err().kind();
    let data = data.as_bytes();
    assert_eq!(kind(verify(&keys(&[RFC_KEY]), data, sig)), io::ErrorKind::InvalidData);
    assert_eq!(kind(verify(&keys(&[KEY]), data, "not hex!")), io::ErrorKind::InvalidData);
    // The signature is checked before the feed is read.
    let e = read_feed(&format!("{{\nsig {sig}\n")).unwrap_err();
    assert!(e.to_string().contains("signature"), "{e}");
  }

  #[test]
  fn a_release_verifies_its_file() {
    let root = dir("verify");
    write(&root, &[("abc", "abc"), ("abd", "abd")]);
    assert_eq!(sha512(&root.join("abc")).unwrap(), hex(ABC).unwrap());
    let r = release(FEED, "1.0", "linux-x86_64").unwrap().unwrap();
    r.verify(&root.join("abc")).unwrap();
    assert_eq!(r.verify(&root.join("abd")).unwrap_err().kind(), io::ErrorKind::InvalidData);
    fs::remove_dir_all(root).unwrap();
  }
}
