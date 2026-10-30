// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update feed, `update.txt` (the readme's Feed), signed by Ed25519 in process, and the
//! private keys that sign it: read from a file or `$SHIPO_UPDATE_KEY`, or made by `keygen`.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use ed25519_compact::sha512::Hash;
use ed25519_compact::{KeyPair, Seed};

use crate::tool::{self, base64, hex};
use crate::{at, bad};

/// The most bytes a feed may be: the apps read no more.
const MAX: usize = 1 << 20;

/// The feed's platforms in the order it lists them, each with the end of its file's name; a
/// `.deb` is apt's, so not among them.
const PLATFORMS: [(&str, &str); 6] = [
  ("macos-x86_64", "-macos-x86_64.dmg"),
  ("macos-aarch64", "-macos-aarch64.dmg"),
  ("windows-x86_64", "-windows-x64-setup.exe"),
  ("windows-aarch64", "-windows-arm64-setup.exe"),
  ("linux-x86_64", "-linux-x86_64.AppImage"),
  ("linux-aarch64", "-linux-aarch64.AppImage"),
];

/// The DER of a PKCS#8 Ed25519 private key (RFC 8410) up to its 32-byte seed.
const PKCS8: [u8; 16] = *b"\x30\x2e\x02\x01\x00\x30\x05\x06\x03\x2b\x65\x70\x04\x22\x04\x20";

/// A private key, the Ed25519 pair of its 32-byte seed. No `Debug`, so no log prints it.
pub struct Key(KeyPair);

impl Key {
  /// The key of `text`, with whitespace around: its seed in 64 hex digits, or the PEM of its
  /// PKCS#8 as `openssl genpkey -algorithm ed25519` writes it; `from` names it in the error,
  /// which never holds the text.
  pub fn parse(text: &str, from: &str) -> io::Result<Self> {
    let text = text.trim();
    match unhex(text).or_else(|| pem(text)) {
      Some(seed) => Ok(Self(KeyPair::from_seed(Seed::new(seed)))),
      None => Err(bad(format!("{from} is not a private key: its seed in 64 hex digits"))),
    }
  }

  /// The key in `file`.
  pub fn read(file: &Path) -> io::Result<Self> {
    Self::parse(&fs::read_to_string(file).map_err(at(file))?, &file.display().to_string())
  }

  /// The key in `file` when given, else in `env`, `$SHIPO_UPDATE_KEY`'s value; an empty one is
  /// none, as a CI secret that is not set comes.
  pub fn load(file: Option<&Path>, env: Option<OsString>) -> io::Result<Self> {
    if let Some(file) = file {
      return Self::read(file);
    }

    let env = env.filter(|v| !v.to_string_lossy().trim().is_empty());
    let env =
      env.ok_or_else(|| bad("no private key: give --key <file> or set $SHIPO_UPDATE_KEY"))?;
    Self::parse(env.to_str().unwrap_or_default(), "$SHIPO_UPDATE_KEY")
  }

  /// The public key, 64 hex digits, as the app holds it.
  pub fn public(&self) -> String {
    hex(&self.0.pk[..])
  }

  /// The signature of `data`, RFC 8032's (no noise), in 128 hex digits.
  fn sign(&self, data: &[u8]) -> String {
    hex(&self.0.sk.sign(data, None)[..])
  }
}

/// The index in [PLATFORMS] of the file named `name`, `<Name>-<label>-<platform>.<ext>`.
fn platform(name: &str) -> Option<usize> {
  PLATFORMS.iter().position(|(_, end)| name.len() > end.len() && name.ends_with(end))
}

/// The feed's records before its signature: `version`, `notes` if given, and a line per file of
/// `files`, each served at `<url>/<its file name>` percent-encoded, `{version}` in `url` the
/// version.
pub fn body(
  version: &str,
  notes: Option<&str>,
  url: &str,
  files: &[PathBuf],
) -> io::Result<String> {
  if !comparable(version) {
    return Err(bad(format!("the version {version:?} is not <number>.<number>… without a `v`")));
  }

  if let Some(n) = notes.filter(|n| !secure(n)) {
    return Err(bad(format!("the notes {n:?} are not one https URL")));
  }

  if files.is_empty() {
    return Err(bad("the feed needs a file"));
  }

  let base = url.replace("{version}", version);
  let base = base.trim_end_matches('/');
  let mut found: [Option<(&Path, String)>; PLATFORMS.len()] = Default::default();
  for file in files {
    let file_name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let Some(i) = platform(file_name) else {
      let ends = PLATFORMS.map(|(_, end)| end).join(", ");
      return Err(bad(format!(
        "{}: not a file of the feed, <Name>-<label>{{{ends}}}",
        file.display()
      )));
    };

    let url = format!("{base}/{}", encode(file_name));
    if !secure(&url) {
      return Err(bad(format!("{url:?} is not an https URL the apps would take")));
    }

    if found[i].replace((file, url)).is_some() {
      return Err(bad(format!("two files for {}", PLATFORMS[i].0)));
    }
  }

  let mut text = String::new();
  writeln!(text, "version {version}").unwrap();
  if let Some(notes) = notes {
    writeln!(text, "notes {notes}").unwrap();
  }

  for ((platform, _), file) in PLATFORMS.iter().zip(found) {
    let Some((path, url)) = file else { continue };
    let data = tool::read(path)?;
    writeln!(text, "{platform} {} {} {url}", data.len(), hex(&Hash::hash(&data))).unwrap();
  }

  Ok(text)
}

/// Writes `dir`/update.txt, `body` and its `sig` line by `key`, and gives its path: written
/// beside it and renamed into place, so the apps never fetch half a feed.
pub fn write(key: &Key, body: &str, dir: &Path) -> io::Result<PathBuf> {
  let text = format!("{body}sig {}\n", key.sign(body.as_bytes()));
  if text.len() > MAX {
    return Err(bad(format!("the feed is {} bytes, over the 1 MiB the apps read", text.len())));
  }

  fs::create_dir_all(dir)?;
  let path = dir.join("update.txt");
  tool::place(&path, |part| {
    File::create_new(part).and_then(|mut out| out.write_all(text.as_bytes())).map_err(at(part))
  })?;
  Ok(path)
}

/// A new key in each of `files`, its seed in hex, readable by the user alone, and their public
/// keys. All or none: a file that is there, or one given twice, stops it before any is written,
/// and a failure removes the files it made.
pub fn keygen(files: &[PathBuf]) -> io::Result<Vec<String>> {
  if let Some((_, file)) = files.iter().enumerate().find(|(i, f)| files[..*i].contains(f)) {
    return Err(bad(format!("{} is given twice", file.display())));
  }

  let mut seeds = vec![[0; 32]; files.len()];
  for seed in &mut seeds {
    getrandom::fill(seed).map_err(|e| io::Error::other(format!("the OS's randomness: {e}")))?;
  }

  let mut keys = vec![];
  for (i, (file, seed)) in files.iter().zip(&seeds).enumerate() {
    if let Err(e) = tool::private(file, format!("{}\n", hex(seed)).as_bytes()) {
      files[..i].iter().for_each(|made| drop(fs::remove_file(made)));
      return Err(match e.kind() {
        io::ErrorKind::AlreadyExists => {
          bad(format!("{} is there: a key is never written over a file", file.display()))
        }
        _ => e,
      });
    }

    keys.push(Key(KeyPair::from_seed(Seed::new(*seed))).public());
  }

  Ok(keys)
}

/// `version` as the apps compare it: numeric dot parts, then an optional
/// `-<pre-release>` and `+<build>`, one word, no `v`.
fn comparable(version: &str) -> bool {
  let word = version.bytes().all(|b| b.is_ascii_graphic());
  let core = version.split(['-', '+']).next().unwrap_or_default();
  let num = |n: &str| n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u64>().is_ok();
  word && core.split('.').all(num)
}

/// `name` percent-encoded for a URL's path: all but RFC 3986's unreserved characters, as UTF-8.
fn encode(name: &str) -> String {
  let keep = |b: u8| b.is_ascii_alphanumeric() || b"-._~".contains(&b);
  name
    .bytes()
    .map(|b| if keep(b) { (b as char).to_string() } else { format!("%{b:02X}") })
    .collect()
}

/// `url` as the apps take it: https to a host, or http to loopback, all printable ASCII.
fn secure(url: &str) -> bool {
  let (scheme, rest) = url.split_once("://").unwrap_or_default();
  let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
  let to = match scheme {
    "https" => !host.is_empty(),
    "http" => {
      let port = host.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()));
      ["127.0.0.1", "localhost", "[::1]"].contains(&port.map_or(host, |(h, _)| h))
    }
    _ => false,
  };

  to && url.bytes().all(|b| b.is_ascii_graphic())
}

/// The seed in `text`, the PEM of a PKCS#8 Ed25519 private key.
fn pem(text: &str) -> Option<[u8; 32]> {
  let body = text.strip_prefix("-----BEGIN PRIVATE KEY-----")?;
  let der = base64(body.strip_suffix("-----END PRIVATE KEY-----")?)?;
  der.strip_prefix(&PKCS8)?.try_into().ok()
}

/// `N` bytes from exactly `2 * N` hex digits, of either case.
fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
  if s.len() != 2 * N || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
    return None;
  }

  let bytes = (0..N).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok());
  bytes.collect::<Option<Vec<u8>>>()?.try_into().ok()
}

#[cfg(test)]
mod tests {
  use std::process::Command;

  use super::*;
  use crate::tool::Temp;

  /// RFC 8032's test 1: the seed and its public key.
  const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
  const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
  /// The seed's PKCS#8 in base64, the body of its PEM.
  const SEED_PKCS8: &str = "MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g";
  /// The SHA-512 of `abc` (FIPS 180-2).
  const ABC: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

  /// The paths of `names` in `dir`.
  fn paths(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|n| dir.join(n)).collect()
  }

  /// A file per platform, `App-dev-…`, each `abc`, and the folder holding them.
  fn release() -> (Temp, Vec<PathBuf>) {
    let names = PLATFORMS.map(|(_, end)| format!("App-dev{end}"));
    let dir = Temp::files(&names.each_ref().map(|n| (n.as_str(), "abc")));
    let files = names.iter().map(|n| dir.join(n)).collect();
    (dir, files)
  }

  fn key() -> Key {
    Key::parse(SEED, "test").unwrap()
  }

  /// The feed v0.1's packer wrote of these files, signed by RFC 8032's test key.
  const V0_1: &str = include_str!("../../tests/fixtures/update-v0.1.txt");

  /// The packer still writes the feed v0.1's apps read, byte for byte; a change here must keep
  /// them reading it (a record of a new first word, which they skip), with the person's approval.
  #[test]
  fn compat_the_feed_v0_1_apps_read() {
    let names = PLATFORMS.map(|(_, end)| format!("App-v1.2.0{end}"));
    let dir = Temp::files(&names.each_ref().map(|n| (n.as_str(), "abc")));
    let notes = "https://github.com/me/app/releases/tag/v1.2.0";
    let url = "https://github.com/me/app/releases/download/v{version}/";
    let text = body("1.2.0", Some(notes), url, &names.map(|n| dir.join(n))).unwrap();
    let path = write(&key(), &text, &dir.join("distrib")).unwrap();
    assert_eq!(path, dir.join("distrib/update.txt"));
    assert_eq!(fs::read_dir(dir.join("distrib")).unwrap().count(), 1, "no part left");
    assert_eq!(fs::read_to_string(&path).unwrap(), V0_1);
  }

  /// The feed the packer writes now, read by v0.1.0's own reader for each platform: what an app
  /// installed with v0.1 takes from it.
  #[test]
  fn compat_v0_1_apps_read_the_feed() {
    let names = PLATFORMS.map(|(_, end)| format!("App-v2.0.0{end}"));
    let dir = Temp::files(&names.each_ref().map(|n| (n.as_str(), "abc")));
    let notes = "https://h/releases/tag/v2.0.0";
    let files = names.map(|n| dir.join(n));
    let text = body("2.0.0", Some(notes), "https://h/{version}", &files).unwrap();
    let feed = fs::read(write(&key(), &text, &dir).unwrap()).unwrap();
    for (platform, end) in PLATFORMS {
      let r = crate::feed_v0_1::read(&[PUBLIC], &feed, "1.0.0", platform).unwrap().unwrap();
      assert_eq!((r.version.as_str(), r.notes.as_deref(), r.size), ("2.0.0", Some(notes), 3));
      let url = format!("https://h/2.0.0/App-v2.0.0{end}");
      assert_eq!((r.url, hex(&r.sha512)), (url, ABC.into()), "{platform}");
    }

    let up_to_date = crate::feed_v0_1::read(&[PUBLIC], &feed, "2.0.0", "linux-x86_64");
    assert_eq!(up_to_date.unwrap(), None);
  }

  #[test]
  fn the_signer_is_rfc_8032s() {
    // RFC 8032's test 1: the empty message.
    let sig = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";
    assert_eq!((key().sign(b"").as_str(), key().public().as_str()), (sig, PUBLIC));
  }

  #[test]
  fn files_name_their_platform() {
    let cases = [
      ("App-dev-macos-x86_64.dmg", Some("macos-x86_64")),
      ("App-v1.2.0-macos-aarch64.dmg", Some("macos-aarch64")),
      ("My App-dev-windows-x64-setup.exe", Some("windows-x86_64")),
      ("App-dev-windows-arm64-setup.exe", Some("windows-aarch64")),
      ("App-linux-x86_64-dev-linux-x86_64.AppImage", Some("linux-x86_64")),
      ("App-dev-linux-aarch64.AppImage", Some("linux-aarch64")),
      ("App-dev-linux-x86_64.deb", None),
      ("App-dev-linux-x86_64.appimage", None),
      ("App-dev-windows-x86_64-setup.exe", None),
      ("App-dev-windows-x64.exe", None),
      ("App-dev-macos-arm64.dmg", None),
      ("App-dev-macos-aarch64.dmg.zip", None),
      ("-macos-aarch64.dmg", None),
      ("", None),
    ];

    for (name, want) in cases {
      assert_eq!(platform(name).map(|i| PLATFORMS[i].0), want, "{name}");
    }
  }

  #[test]
  fn version_in_the_url() {
    let (_dir, files) = release();
    let text = body("2.0.0", None, "https://h/{version}/x/{version}", &files).unwrap();
    let line = text.lines().find(|l| l.starts_with("linux-aarch64 ")).unwrap();
    assert!(line.ends_with(" https://h/2.0.0/x/2.0.0/App-dev-linux-aarch64.AppImage"), "{line}");
    let order = text.lines().map(|l| l.split(' ').next().unwrap()).collect::<Vec<_>>();
    let want = ["version"].into_iter().chain(PLATFORMS.map(|(p, _)| p)).collect::<Vec<_>>();
    assert_eq!(order, want);
  }

  #[test]
  fn errors() {
    let (dmg, deb) = ("App-dev-macos-aarch64.dmg", "App-dev-linux-x86_64.deb");
    let dir = Temp::files(&[(dmg, "a"), (deb, "b")]);
    let ok = "https://h";
    let cases = [
      ("1.0", None, ok, &[deb][..], "not a file of the feed"),
      ("1.0", None, ok, &[dmg, dmg], "two files for macos-aarch64"),
      ("1.0", None, ok, &[], "the feed needs a file"),
      ("1.0", None, ok, &["App-dev-linux-x86_64.AppImage"], "App-dev-linux-x86_64.AppImage: "),
      ("1.0", None, "http://example.com", &[dmg], "not an https URL"),
      ("1.0", None, "https://", &[dmg], "not an https URL"),
      ("1.0", None, "https://h/a b", &[dmg], "not an https URL"),
      ("1.0", None, "ftp://h", &[dmg], "not an https URL"),
      ("1.0", Some("http://h/notes"), ok, &[dmg], "notes"),
      ("1.0", Some("https://h/a b"), ok, &[dmg], "notes"),
      ("latest", None, ok, &[dmg], "version"),
    ];

    for (version, notes, url, names, want) in cases {
      let e = body(version, notes, url, &paths(&dir, names)).unwrap_err();
      assert!(e.to_string().contains(want), "{version} {notes:?} {url} {names:?}: {e}");
    }

    let text = body("1.0", None, "http://127.0.0.1:8000/", &paths(&dir, &[dmg]));
    assert!(text.unwrap().contains(" http://127.0.0.1:8000/App-dev-macos-aarch64.dmg\n"));
  }

  /// The PEM of `label` holding the base64 `body`, its lines ending in `end`.
  fn pem_of(label: &str, body: &str, end: &str) -> String {
    format!("-----BEGIN {label}-----{end}{body}{end}-----END {label}-----{end}")
  }

  #[test]
  fn keys_parse() {
    let upper = format!("\n  {}\r\n", SEED.to_uppercase());
    let pem = pem_of("PRIVATE KEY", SEED_PKCS8, "\n");
    let crlf = pem.replace('\n', "\r\n");
    for text in [SEED, &format!("{SEED}\n"), &upper, &pem, &crlf] {
      assert_eq!(Key::parse(text, "k").unwrap().public(), PUBLIC, "{text:?}");
    }

    let secret = &SEED[..62];
    let mut wrong =
      [String::new(), secret.into(), format!("{SEED}0"), format!("{secret}xy")].to_vec();
    wrong.extend([format!("{SEED} {SEED}"), pem[..pem.len() - 2].into()]);
    // The public key's PEM, X25519's private key, a seed short by a byte, not base64.
    let pems = [
      ("PUBLIC KEY", "MCowBQYDK2VwAyEA11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo="),
      ("PRIVATE KEY", "MC4CAQAwBQYDK2VuBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9g"),
      ("PRIVATE KEY", "MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn8="),
      ("PRIVATE KEY", "MC4CAQAwBQYDK2VwBCIEIJ1hsZ3v/VpguoRK9JLsLMREScVpezJpGXA7rAMcrn9!"),
    ];
    wrong.extend(pems.map(|(label, body)| pem_of(label, body, "\n")));
    for text in &wrong {
      let e = Key::parse(text, "k").err().unwrap().to_string();
      assert_eq!(e, "k is not a private key: its seed in 64 hex digits");
    }

    let dir = Temp::files(&[("a.key", SEED), ("bad.key", secret)]);
    let env = || Some(OsString::from(SEED));
    assert_eq!(Key::load(Some(&dir.join("a.key")), None).unwrap().public(), PUBLIC);
    assert_eq!(Key::load(None, env()).unwrap().public(), PUBLIC);
    let e = |file: Option<&Path>, env| Key::load(file, env).err().unwrap().to_string();
    let bad = dir.join("bad.key");
    assert_eq!(
      e(Some(&bad), env()),
      format!("{} is not a private key: its seed in 64 hex digits", bad.display())
    );
    assert!(
      e(Some(&dir.join("none.key")), env())
        .starts_with(&format!("{}: ", dir.join("none.key").display()))
    );
    let none = "no private key: give --key <file> or set $SHIPO_UPDATE_KEY";
    for env in [None, Some(OsString::new()), Some(OsString::from(" \n"))] {
      assert_eq!(e(None, env), none);
    }

    assert_eq!(
      e(None, Some(secret.into())),
      "$SHIPO_UPDATE_KEY is not a private key: its seed in 64 hex digits"
    );
  }

  /// A key `openssl genpkey` made, when this host's OpenSSL makes Ed25519 keys (macOS's LibreSSL
  /// doesn't): its public half is OpenSSL's.
  #[test]
  fn openssl_keys_parse() {
    let tmp = Temp::new().unwrap();
    let file = tmp.join("k.pem");
    let openssl = |args: &str| tool::run(Command::new("openssl").args(args.split(' ')).arg(&file));
    if openssl("genpkey -algorithm ed25519 -out").is_err() {
      return;
    }

    let der = openssl("pkey -pubout -outform DER -in").unwrap();
    assert_eq!(Key::read(&file).unwrap().public(), hex(&der[der.len() - 32..]));
  }

  /// The platform of this host, as the library reads the feed.
  fn host() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
  }

  #[test]
  fn the_library_reads_the_feed() {
    let (dir, files) = release();
    let notes = "https://h/releases/tag/v1.2.0";
    let text = body("1.2.0", Some(notes), "https://h/{version}", &files).unwrap();
    let feed = fs::read(write(&key(), &text, &dir).unwrap()).unwrap();
    let reader = shipo::Feed::new("https://h/update.txt", &[PUBLIC]);
    let r = reader.read(&feed, "0.0.0").unwrap();
    match PLATFORMS.iter().find(|(p, _)| *p == host()) {
      Some((_, end)) => {
        let r = r.unwrap();
        assert_eq!((r.version.as_str(), r.notes.as_deref()), ("1.2.0", Some(notes)));
        assert_eq!(
          (r.url, r.size, hex(&r.sha512)),
          (format!("https://h/1.2.0/App-dev{end}"), 3, ABC.into())
        );
      }
      None => assert_eq!(r, None, "no file for this host"),
    }

    assert_eq!(reader.read(&feed, "1.2.0").unwrap(), None, "up to date");
    let other = Key::parse(&"11".repeat(32), "other").unwrap().public();
    let theirs = shipo::Feed::new("https://h/update.txt", &[&other]).read(&feed, "0.0.0");
    assert!(theirs.is_err(), "another key");
    // The feed ends in `sig <128 hex>\n`, 133 bytes: before them the last record's LF, and its
    // URL before that; the last of them a digit of the signature.
    for at in [0, 9, feed.len() - 140, feed.len() - 134, feed.len() - 2] {
      let mut changed = feed.clone();
      changed[at] ^= 1;
      assert!(reader.read(&changed, "0.0.0").is_err(), "byte {at} changed");
    }
  }

  #[test]
  fn urls_are_checked_as_the_library_does() {
    if !PLATFORMS.iter().any(|(p, _)| *p == host()) {
      return;
    }

    let reader = shipo::Feed::new("https://h/update.txt", &[PUBLIC]);
    let urls = [
      "https://a/f",
      "https://a",
      "https://a:8443/f?x#y",
      "http://127.0.0.1:8000/f",
      "http://localhost/f",
      "http://[::1]:80",
      "http://example.com/f",
      "http://127.0.0.1.evil.com/f",
      "http://127.0.0.1@evil.com/f",
      "https://",
      "https:///f",
      "https://?f",
      "ftp://localhost/f",
      "https://a b",
      "https://a\tb",
      "https://é",
    ];

    for url in urls {
      let text = format!("version 2\n{} 3 {ABC} {url}\n", host());
      let feed = format!("{text}sig {}\n", key().sign(text.as_bytes()));
      let read = reader.read(feed.as_bytes(), "1").is_ok();
      assert_eq!(secure(url), read, "{url:?}");
    }
  }

  #[test]
  fn versions_the_apps_compare() {
    for v in ["1", "1.2.0", "1.2.0-rc.1", "1.2.0-0.3.7", "1.2.0+5", "1.2.0-rc.1+b.7", "01.2"] {
      assert!(comparable(v), "{v}");
    }

    let refused =
      ["", "latest", "1.x", "1..0", ".1", "1.", "1.0 beta", "1.0\t", "-1", "+1", "1.+2"];
    for v in refused.into_iter().chain(["99999999999999999999", "1.2é", "v1.0", "V1.0"]) {
      assert!(!comparable(v), "{v:?}");
    }

    // The library takes the same, but for a leading `v`, which the packer keeps out of the feed.
    let reader = shipo::Feed::new("https://h/update.txt", &[PUBLIC]);
    for v in ["1", "1.2.0-rc.1+b.7", "latest", "1.x", "1..0", "1.0 beta", "99999999999999999999"] {
      let text = format!("version {v}\n");
      let feed = format!("{text}sig {}\n", key().sign(text.as_bytes()));
      assert_eq!(reader.read(feed.as_bytes(), "0.0.0").is_ok(), comparable(v), "{v:?}");
    }
  }

  #[test]
  fn names_are_percent_encoded() {
    assert_eq!(encode("Wavö App-dev-a_b~c.dmg"), "Wav%C3%B6%20App-dev-a_b~c.dmg");
    assert_eq!(encode("a+b%c/d?#"), "a%2Bb%25c%2Fd%3F%23");
    let Some((_, end)) = PLATFORMS.iter().find(|(p, _)| *p == host()) else { return };
    let (space, other) = (format!("My App-dev{end}"), format!("Wavö-dev{end}"));
    let dir = Temp::files(&[(&space, "abc"), (&other, "abc")]);
    let reader = shipo::Feed::new("https://h/update.txt", &[PUBLIC]);
    for (name, want) in
      [(space, format!("My%20App-dev{end}")), (other, format!("Wav%C3%B6-dev{end}"))]
    {
      let text = body("1.0", None, "https://h/", &paths(&dir, &[&name])).unwrap();
      let feed = fs::read(write(&key(), &text, &dir.join("out")).unwrap()).unwrap();
      let r = reader.read(&feed, "0.0.0").unwrap().unwrap();
      assert_eq!(r.url, format!("https://h/{want}"));
    }
  }

  #[test]
  fn writes_a_feed_alone() {
    let dir = Temp::files(&[("target", "old")]);
    let out = dir.join("out");
    let e = write(&key(), &"x".repeat(MAX), &out).unwrap_err();
    assert!(e.to_string().contains("over the 1 MiB"), "{e}");
    assert!(!out.exists(), "nothing written");
    let text = "version 1\n";
    write(&key(), text, &out).unwrap();
    let small = fs::read(out.join("update.txt")).unwrap();
    assert_eq!(small.len(), text.len() + 133);
    // The longest feed the apps read.
    let longest = "x".repeat(MAX - 133);
    assert_eq!(fs::read(write(&key(), &longest, &out).unwrap()).unwrap().len(), MAX);
  }

  #[test]
  fn keygen_writes_new_keys_alone() {
    let dir = Temp::files(&[("there.key", "x")]);
    let files = paths(&dir, &["a.key", "b.key"]);
    let keys = keygen(&files).unwrap();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    for (file, public) in files.iter().zip(&keys) {
      let text = fs::read_to_string(file).unwrap();
      assert!(text.len() == 65 && text.ends_with('\n'), "{text:?}");
      assert!(unhex::<32>(text.trim_end()).is_some(), "{text:?}");
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
      }
      assert_eq!(&Key::read(file).unwrap().public(), public);
      assert!(unhex::<32>(public).is_some());
    }

    let cases = [
      (&["c.key", "a.key"][..], "is there: a key is never written over a file"),
      (&["c.key", "there.key"], "is there: a key is never written over a file"),
      (&["c.key", "d.key", "c.key"], "c.key is given twice"),
      (&["c.key", "d.key", "none/e.key"], "e.key: "),
    ];

    for (names, want) in cases {
      let e = keygen(&paths(&dir, names)).unwrap_err();
      assert!(e.to_string().contains(want), "{names:?}: {e}");
      for name in ["c.key", "d.key"] {
        assert!(!dir.join(name).exists(), "{names:?}: {name} left");
      }
    }

    assert_eq!(fs::read_dir(&*dir).unwrap().count(), 3, "a.key, b.key and there.key alone");

    // The path once: `tool::private` names it.
    let missing = dir.join("none/e.key");
    let e = keygen(std::slice::from_ref(&missing)).unwrap_err().to_string();
    assert_eq!(e.matches(&*missing.to_string_lossy()).count(), 1, "{e}");

    assert_eq!(fs::read_to_string(dir.join("there.key")).unwrap(), "x");
  }
}
