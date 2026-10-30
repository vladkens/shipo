// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! Signing and notarization of the macOS app and its DMG: on a Mac by `codesign` with the
//! keychain's identity, made when it must be; elsewhere by rcodesign, fetched pinned, with the
//! Developer ID's `.p12` from the release job's secrets; and Apple's notary service.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, fs, io};

use crate::config::Config;
use crate::tool::{self, Temp, private, run, run_until};
use crate::{bad, wrap};

/// `path`, a bundle or an image, once `finish` signed it, or notarized it too, with what that
/// gives; removed when it fails, so nothing unsigned is left among a release's files, nor a bundle
/// signed ad hoc on a Mac, whose grants would last that build only.
pub fn kept<T>(
  path: PathBuf,
  finish: impl FnOnce(&Path) -> io::Result<T>,
) -> io::Result<(PathBuf, T)> {
  match finish(&path) {
    Ok(x) => Ok((path, x)),
    Err(e) => {
      let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
      Err(wrap(&e, format!("{}: {e}; removed", path.display())))
    }
  }
}

// MARK: Signing on a Mac

/// What signs the app on a Mac, as [choose] says, "<Name> Local Signing" made when it must be.
pub fn identity(cfg: &Config, tag: Option<&str>) -> io::Result<Identity> {
  let find = |valid: bool| -> io::Result<String> {
    let mut cmd = Command::new("security");
    cmd.arg("find-identity");
    if valid {
      cmd.arg("-v");
    }

    Ok(String::from_utf8_lossy(&run(cmd.args(["-p", "codesigning"]))?).into_owned())
  };

  let local = format!("{} Local Signing", cfg.name);
  let id = choose(&find(true)?, &find(false)?, &local, tag)?;
  if id.make {
    make_identity(&local)?;
    if choose("", &find(false)?, &local, None)?.make {
      return Err(io::Error::other(format!(
        "made \"{local}\", but `security find-identity` does not list it"
      )));
    }
  }

  Ok(id)
}

/// A code signing identity in the keychain.
#[derive(Debug, PartialEq)]
pub struct Identity {
  /// What `codesign --sign` takes.
  pub id: String,
  /// How it is said.
  pub name: String,
  /// A Developer ID, whose signature takes Apple's secure timestamp.
  pub developer: bool,
  /// Not in the keychain yet: [make_identity] makes it.
  make: bool,
}

/// What signs the app, of `security find-identity -p codesigning`'s outputs, `valid` with `-v` and
/// `all` without: the first Developer ID, by its hash (two of one name, a renewal, are ambiguous by
/// name); else `local`, the self-signed identity, by its name, which a release (a `tag`) refuses.
/// The local one's designated requirement names the bundle id and the certificate, not the code's
/// hash, so the grants macOS keeps survive rebuilds.
fn choose(valid: &str, all: &str, local: &str, tag: Option<&str>) -> io::Result<Identity> {
  let quoted = |out: &str| -> Vec<(String, String)> {
    let lines = out.lines().filter_map(|ln| {
      let hash = ln.split_whitespace().nth(1)?;
      let name = ln.split('"').nth(1)?;
      Some((hash.to_string(), name.to_string()))
    });

    lines.collect()
  };

  let developer =
    quoted(valid).into_iter().find(|(_, n)| n.starts_with("Developer ID Application:"));
  if let Some((id, name)) = developer {
    return Ok(Identity { id, name, developer: true, make: false });
  }

  if let Some(tag) = tag {
    return Err(bad(format!(
      "{tag} is a release, signed by a Developer ID: the keychain has none"
    )));
  }

  let make = !quoted(all).iter().any(|(_, n)| n == local);
  Ok(Identity { id: local.into(), name: local.into(), developer: false, make })
}

/// A self-signed code signing certificate named `name` in the login keychain, codesign allowed
/// to use its key.
fn make_identity(name: &str) -> io::Result<()> {
  let tmp = Temp::new()?;
  let (key, cert, p12) = (tmp.join("key.pem"), tmp.join("cert.pem"), tmp.join("identity.p12"));
  // macOS's own openssl (LibreSSL): `security import` reads its PKCS#12, not OpenSSL 3's.
  let openssl = || Command::new("/usr/bin/openssl");
  let subject = format!("/CN={name}");
  let mut new = openssl();
  new.args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-subj", &subject]);
  new.args(["-addext", "keyUsage=critical,digitalSignature"]);
  new.args(["-addext", "extendedKeyUsage=critical,codeSigning"]);
  run(new.arg("-keyout").arg(&key).arg("-out").arg(&cert))?;
  let mut export = openssl();
  export.args(["pkcs12", "-export", "-inkey"]).arg(&key).arg("-in").arg(&cert);
  run(export.arg("-out").arg(&p12).args(["-passout", "pass:shipo"]))?;

  let home = env::var_os("HOME").ok_or_else(|| bad("no $HOME, the login keychain's"))?;
  let keychain = Path::new(&home).join("Library/Keychains/login.keychain-db");
  let mut import = Command::new("security");
  import.arg("import").arg(&p12).arg("-k").arg(keychain);
  run(import.args(["-P", "shipo", "-T", "/usr/bin/codesign"]))?;
  eprintln!("shipo: made the code signing identity \"{name}\" in the login keychain");
  Ok(())
}

/// `codesign`'s arguments to sign with `id` (`-` is ad hoc), the hardened runtime and
/// `entitlements`: a `developer` ID's signature takes Apple's secure timestamp, which notarization
/// needs; the local one's none, so it signs offline.
pub fn codesign_args(id: &str, developer: bool, entitlements: Option<&Path>) -> Vec<OsString> {
  let stamp = if developer { "--timestamp" } else { "--timestamp=none" };
  let mut args: Vec<OsString> =
    ["--force", "--sign", id, "--options", "runtime", stamp].map(OsString::from).into();
  if let Some(path) = entitlements {
    args.extend(["--entitlements".into(), path.into()]);
  }

  args
}

/// Signs `bundle` by `codesign` with `args`, then verifies it. Its extended attributes go first:
/// codesign refuses a bundle with Finder info. A keychain prompt nobody answers ends it after a
/// minute.
pub fn codesign(bundle: &Path, args: &[OsString]) -> io::Result<()> {
  run(Command::new("xattr").arg("-cr").arg(bundle))?;
  run_until(Command::new("codesign").args(args).arg(bundle), Duration::from_secs(60))?;
  run(Command::new("codesign").args(["--verify", "--strict"]).arg(bundle))?;
  Ok(())
}

// MARK: Signing elsewhere

/// The secrets of the Developer ID certificate: a base64 `.p12` and its password.
pub const CERT: [&str; 2] = ["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"];

/// rcodesign (apple-codesign 0.29.0, 2024-11-29) by the triple it runs on, and its tarball's
/// SHA-256 as the release's SHA256SUMS gives it.
const RCODESIGN: &str = concat!(
  "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/0.29.0/",
  "apple-codesign-0.29.0-{triple}.tar.gz",
);
const RCODESIGN_SHA256: [(&str, &str); 3] = [
  ("x86_64-unknown-linux-musl", "dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002"),
  (
    "aarch64-unknown-linux-musl",
    "4af92c87ddf52f5f2d1258a3b4e56c7dcb8f1b2468df744976c5f139e031961f",
  ),
  ("aarch64-apple-darwin", "d1a532150adaf90048260d76359261aa716abafc45c53c5dc18845029184334a"),
];

/// Whether each of `names` is set in the environment; with a release's `tag`, one missing is an
/// error.
pub fn secrets(names: &[&str], tag: Option<&str>) -> io::Result<bool> {
  let missing = names.iter().copied().filter(|n| env::var_os(n).is_none_or(|v| v.is_empty()));
  let missing: Vec<_> = missing.collect();
  if let Some(tag) = tag
    && !missing.is_empty()
  {
    return Err(bad(format!(
      "{tag} is a release, signed and notarized: set {}",
      missing.join(", ")
    )));
  }

  Ok(missing.is_empty())
}

/// rcodesign for this host, its tarball fetched into `tools` once, pinned by SHA-256.
pub fn rcodesign(tools: &Path) -> io::Result<PathBuf> {
  let os = match env::consts::OS {
    "macos" => "apple-darwin",
    _ => "unknown-linux-musl",
  };

  let triple = format!("{}-{os}", env::consts::ARCH);
  let Some(&(_, sha256)) = RCODESIGN_SHA256.iter().find(|(t, _)| *t == triple) else {
    return Err(bad(format!("no rcodesign is pinned for {triple}")));
  };

  let url = RCODESIGN.replace("{triple}", &triple);
  let name = url.rsplit('/').next().unwrap_or_default();
  let tarball = tool::fetch(&url, sha256, &tools.join(name))?;
  let member = format!("apple-codesign-0.29.0-{triple}/rcodesign");
  tool::untar(&tarball, &member, &tools.join("rcodesign"))
}

/// Signs `bundle` by rcodesign with the hardened runtime and `entitlements`: `signed` by the
/// Developer ID of [CERT] for notarization, else ad hoc. Gives who signed it.
pub fn sign_elsewhere(
  cfg: &Config,
  bundle: &Path,
  signed: bool,
  entitlements: Option<&Path>,
) -> io::Result<String> {
  let rcodesign = rcodesign(&cfg.target.join("tools"))?;
  let tmp = Temp::new()?;
  let p12 = if signed { Some(developer_p12(&tmp)?) } else { None };

  rsign(&rcodesign, bundle, p12.as_ref(), &rcodesign_args(signed, entitlements))?;
  Ok(if signed { "Developer ID" } else { "ad hoc" }.into())
}

/// rcodesign's arguments to sign the app with the hardened runtime and `entitlements`, checked
/// for notarization when `signed` by a Developer ID.
fn rcodesign_args(signed: bool, entitlements: Option<&Path>) -> Vec<OsString> {
  let mut args: Vec<OsString> = vec!["--code-signature-flags".into(), "runtime".into()];
  if signed {
    args.push("--for-notarization".into());
  }

  if let Some(path) = entitlements {
    args.extend(["--entitlements-xml-file".into(), path.into()]);
  }

  args
}

/// Signs the disk image `path` as [app] signs the bundle: on a Mac by [identity], elsewhere by
/// rcodesign with the Developer ID of [CERT], else not at all. Gives who signed it, if one did, and
/// whether that is a Developer ID, which notarization takes.
pub fn sign_image(cfg: &Config, path: &Path, tag: Option<&str>) -> io::Result<(String, bool)> {
  if cfg!(target_os = "macos") {
    let id = identity(cfg, tag)?;
    let stamp = if id.developer { "--timestamp" } else { "--timestamp=none" };
    codesign(path, &["--force", "--sign", &id.id, stamp].map(OsString::from))?;
    return Ok((id.name, id.developer));
  }

  if !secrets(&CERT, tag)? {
    return Ok(("no one".into(), false));
  }

  let rcodesign = rcodesign(&cfg.target.join("tools"))?;
  let tmp = Temp::new()?;
  rsign(&rcodesign, path, Some(&developer_p12(&tmp)?), &[])?;
  Ok(("Developer ID".into(), true))
}

/// Signs `path` in place by `rcodesign` with `args`: by the key and certificate in `p12`, else ad
/// hoc.
pub fn rsign(
  rcodesign: &Path,
  path: &Path,
  p12: Option<&P12>,
  args: &[OsString],
) -> io::Result<()> {
  let mut cmd = Command::new(rcodesign);
  cmd.arg("sign");
  if let Some(p12) = p12 {
    cmd.arg("--p12-file").arg(&p12.file).arg("--p12-password-file").arg(&p12.password);
  }

  // rcodesign reads a .p12 of 3DES or RC2 only, and takes one of AES (OpenSSL 3's default) for a
  // wrong password (rcodesign #315), with this error.
  let wrong = "incorrect password given when decrypting PFX data";
  let legacy = |e: io::Error| match p12.is_some() && e.to_string().contains(wrong) {
    true => bad(format!(
      "{e}: if {} is right, export {} again with `openssl pkcs12 -export -legacy`",
      CERT[1], CERT[0]
    )),
    false => e,
  };

  run(cmd.args(args).arg(path)).map_err(legacy)?;
  Ok(())
}

/// A `.p12` and its password, each in a file only its owner reads, as rcodesign takes them.
#[derive(Debug)]
pub struct P12 {
  file: PathBuf,
  password: PathBuf,
}

/// The Developer ID's `.p12` of [CERT] in the folder `dir`.
fn developer_p12(dir: &Path) -> io::Result<P12> {
  certificate(dir, &var(CERT[0])?, &var(CERT[1])?)
}

/// The environment variable `name`, a secret: an error names it, never its value.
fn var(name: &str) -> io::Result<String> {
  env::var(name).map_err(|e| bad(format!("{name}: {e}")))
}

/// The `.p12` in `p12` (base64) and its `password` in the folder `dir`.
pub fn certificate(dir: &Path, p12: &str, password: &str) -> io::Result<P12> {
  let data = base64(p12).ok_or_else(|| bad(format!("{} is not base64", CERT[0])))?;
  let file = private(&dir.join("id.p12"), &data)?;
  let password = private(&dir.join("id.password"), password.as_bytes())?;
  Ok(P12 { file, password })
}

/// The bytes of standard base64 `text`, whitespace skipped, ending at its first `=`.
fn base64(text: &str) -> Option<Vec<u8>> {
  let (mut out, mut acc, mut bits) = (vec![], 0u32, 0);
  for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
    let v = match c {
      b'A'..=b'Z' => c - b'A',
      b'a'..=b'z' => c - b'a' + 26,
      b'0'..=b'9' => c - b'0' + 52,
      b'+' => 62,
      b'/' => 63,
      b'=' => break,
      _ => return None,
    };

    acc = (acc << 6 | v as u32) & 0xFFFF;
    bits += 6;
    if bits >= 8 {
      bits -= 8;
      out.push((acc >> bits) as u8);
    }
  }

  Some(out)
}

// MARK: Notarization

/// The secrets of the App Store Connect API key that notarizes: the `.p8`, its id and the issuer.
pub const NOTARY: [&str; 3] = ["APPLE_API_KEY", "APPLE_API_KEY_ID", "APPLE_API_ISSUER"];

/// Submits `dmg` to Apple's notary service by rcodesign with the API key of [NOTARY], waits up to
/// 20 minutes and staples the ticket. The key goes through files only its owner reads.
pub fn notary(cfg: &Config, dmg: &Path) -> io::Result<()> {
  let rcodesign = rcodesign(&cfg.target.join("tools"))?;
  let tmp = Temp::new()?;
  let p8 = private(&tmp.join("key.p8"), var(NOTARY[0])?.as_bytes())?;
  let mut encode = Command::new(&rcodesign);
  encode.arg("encode-app-store-connect-api-key").args([var(NOTARY[2])?, var(NOTARY[1])?]);
  let key = private(&tmp.join("key.json"), &run(encode.arg(&p8))?)?;
  let mut submit = Command::new(&rcodesign);
  submit.args(["notary-submit", "--staple", "--max-wait-seconds", "1200", "--api-key-file"]);
  let said = run_until(submit.arg(&key).arg(dmg), Duration::from_secs(25 * 60));
  said.map_err(|e| io::Error::new(e.kind(), format!("notarizing {}: {e}", dmg.display())))?;
  Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use crate::macos::bundle;
  use crate::macos::tests::test_app;

  #[test]
  fn identities() {
    let valid = r#"  1) 7D917A56955A0E81554606196D2673E00F9E4A9A "Hello Local Signing"
  2) E00087BDD71F8F29D6E4213BE04F8CF15C0DB969 "Developer ID Application: Jane Doe (ABCDE12345)"
     2 valid identities found
"#;
    let none = "     0 valid identities found\n";
    let untrusted = r#"  1) 7D917A56955A0E81554606196D2673E00F9E4A9A "Hello Local Signing" (CSSMERR_TP_NOT_TRUSTED)
     1 identities found
"#;

    let local = "Hello Local Signing";
    let dev = (
      "E00087BDD71F8F29D6E4213BE04F8CF15C0DB969",
      "Developer ID Application: Jane Doe (ABCDE12345)",
    );
    let other = untrusted.replace("Hello", "Other");
    let id = |id: &str, name: &str, developer, make| {
      Ok(Identity { id: id.into(), name: name.into(), developer, make })
    };

    let got = |valid, all, tag| choose(valid, all, local, tag).map_err(|e| e.to_string());
    assert_eq!(got(valid, valid, None), id(dev.0, dev.1, true, false));
    assert_eq!(got(valid, valid, Some("v1.2.0")), id(dev.0, dev.1, true, false));
    assert_eq!(got(none, untrusted, None), id(local, local, false, false));
    assert_eq!(got(none, none, None), id(local, local, false, true));
    assert_eq!(got(untrusted, &other, None), id(local, local, false, true));
    let refused = "v1.2.0 is a release, signed by a Developer ID: the keychain has none";
    assert_eq!(got(untrusted, untrusted, Some("v1.2.0")), Err(refused.into()));
  }

  #[test]
  fn signing_args() {
    let args = |v: Vec<OsString>| {
      v.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    };
    let ent = Path::new("/app/hello.entitlements");
    let cases = [
      (
        codesign_args("ABC", true, Some(ent)),
        "--force --sign ABC --options runtime --timestamp --entitlements /app/hello.entitlements",
      ),
      (
        codesign_args("Hello Local Signing", false, None),
        "--force --sign Hello Local Signing --options runtime --timestamp=none",
      ),
      (
        rcodesign_args(true, Some(ent)),
        "--code-signature-flags runtime --for-notarization --entitlements-xml-file /app/hello.entitlements",
      ),
      (rcodesign_args(false, None), "--code-signature-flags runtime"),
    ];

    for (got, want) in cases {
      assert_eq!(args(got), want);
    }
  }

  #[test]
  fn base64s() {
    let cases = [
      ("", Some(&b""[..])),
      ("Zg==", Some(b"f")),
      ("Zm8=", Some(b"fo")),
      ("Zm9v", Some(b"foo")),
      ("Zm9v\nYmFy\r\n", Some(b"foobar")),
      ("+/8=", Some(&[0xFB, 0xFF][..])),
      ("Zm9v!", None),
    ];

    for (text, want) in cases {
      assert_eq!(base64(text).as_deref(), want, "{text:?}");
    }
  }

  #[test]
  fn secret_names() {
    let names = ["SHIPO_TEST_UNSET_A", "SHIPO_TEST_UNSET_B"];
    assert!(!secrets(&names, None).unwrap());
    let e = secrets(&names, Some("v1.2.0")).unwrap_err().to_string();
    assert_eq!(
      e,
      "v1.2.0 is a release, signed and notarized: set SHIPO_TEST_UNSET_A, SHIPO_TEST_UNSET_B"
    );
    assert!(secrets(&["PATH"], Some("v1.2.0")).unwrap());
  }

  #[test]
  fn removed_unsigned() {
    let tmp = Temp::new().unwrap();
    let app = tmp.join("Hello.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let (path, name) = kept(app.clone(), |_| Ok("ad hoc")).unwrap();
    assert_eq!((path.as_path(), name), (app.as_path(), "ad hoc"));
    assert!(app.exists());

    let e = kept(app.clone(), |_| Err::<(), _>(io::Error::other("no key"))).unwrap_err();
    assert_eq!(e.to_string(), format!("{}: no key; removed", app.display()));
    assert!(!app.exists());

    // An image, a file.
    let dmg = tmp.join("Hello-dev-macos-aarch64.dmg");
    fs::write(&dmg, b"image").unwrap();
    assert_eq!(kept(dmg.clone(), |_| Ok(())).unwrap().0, dmg);
    let e = kept(dmg.clone(), |_| Err::<(), _>(io::Error::other("notarizing: rejected")));
    assert_eq!(
      e.unwrap_err().to_string(),
      format!("{}: notarizing: rejected; removed", dmg.display())
    );
    assert!(!dmg.exists());
  }

  /// codesign with an identity the keychain lacks fails, and the bundle goes.
  #[cfg(target_os = "macos")]
  #[test]
  fn removed_by_codesign() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let app = bundle(&cfg, &env::current_exe().unwrap()).unwrap();
    let args = codesign_args(&"0".repeat(40), false, None);
    let e = kept(app.clone(), |b| codesign(b, &args)).unwrap_err();
    assert!(e.to_string().contains("codesign failed"), "{e}");
    assert!(!app.exists());
  }

  /// This test binary as the app, signed ad hoc by codesign with the entitlements, the hardened
  /// runtime on, and read back by codesign and plutil.
  #[cfg(target_os = "macos")]
  #[test]
  fn signed_ad_hoc() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let app = bundle(&cfg, &env::current_exe().unwrap()).unwrap();
    codesign(&app, &codesign_args("-", false, Some(&tmp.join("hello.entitlements")))).unwrap();

    run(Command::new("plutil").arg("-lint").arg(app.join("Contents/Info.plist"))).unwrap();
    let out = Command::new("codesign").arg("-dv").arg(&app).output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("Identifier=com.example.hello"), "{said}");
    assert!(said.contains("(adhoc,runtime)"), "{said}");
    let out = run(Command::new("codesign").args(["-d", "--entitlements", "-", "--xml"]).arg(&app));
    let out = String::from_utf8(out.unwrap()).unwrap();
    assert!(out.contains("com.apple.security.device.audio-input"), "{out}");
  }

  /// A self-signed certificate's `.p12`, base64, of the password `pw`, made in `dir`: in OpenSSL's
  /// default ciphers, else in 3DES and SHA-1, as `-legacy` makes it.
  #[cfg(unix)]
  pub fn p12(dir: &Path, pw: &str, legacy: bool) -> String {
    let (key, cert, p12) = (dir.join("k.pem"), dir.join("c.pem"), dir.join("c.p12"));
    let mut req = Command::new("openssl");
    req.args("req -x509 -newkey rsa:2048 -nodes -days 1".split(' ')).args(["-subj", "/CN=shipo"]);
    req.args(["-addext", "keyUsage=critical,digitalSignature"]);
    req.args(["-addext", "extendedKeyUsage=critical,codeSigning"]);
    run(req.arg("-keyout").arg(&key).arg("-out").arg(&cert)).unwrap();
    let mut export = Command::new("openssl");
    export.args(["pkcs12", "-export", "-inkey"]).arg(&key).arg("-in").arg(&cert);
    if legacy {
      export.args("-certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1".split(' '));
    }

    run(export.arg("-out").arg(&p12).args(["-passout", &format!("pass:{pw}")])).unwrap();
    let out = run(Command::new("openssl").args(["base64", "-in"]).arg(&p12)).unwrap();
    String::from_utf8(out).unwrap()
  }

  #[cfg(unix)]
  #[test]
  fn certificate_files() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = Temp::new().unwrap();
    let p12 = certificate(&tmp, "aGVs\nbG8=", "secret pw").unwrap();
    assert_eq!(fs::read(&p12.file).unwrap(), b"hello");
    assert_eq!(fs::read(&p12.password).unwrap(), b"secret pw");
    for path in [&p12.file, &p12.password] {
      assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let e = certificate(&tmp, "not base64!", "x").unwrap_err().to_string();
    assert_eq!(e, "APPLE_CERTIFICATE is not base64");
  }

  /// rcodesign fetched into the target folder's `tools`, signing the app as it does off macOS:
  /// ad hoc, then by a self-signed certificate; on a Mac, codesign verifies both. The binary is
  /// `$SHIPO_TEST_MACHO`, else this test's own.
  #[cfg(unix)]
  #[test]
  #[ignore = "fetches rcodesign from GitHub"]
  fn rcodesign_signs() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let bin =
      env::var_os("SHIPO_TEST_MACHO").map_or_else(|| env::current_exe().unwrap(), PathBuf::from);
    let entitlements = tmp.join("hello.entitlements");
    let check = |app: &Path| {
      if cfg!(target_os = "macos") {
        run(Command::new("codesign").args(["--verify", "--strict"]).arg(app)).unwrap();
      }
    };

    // The target folder of this test's `<target>/debug/deps/<test>`.
    let exe = env::current_exe().unwrap();
    let tools = exe.ancestors().nth(3).unwrap().join("tools");
    let rcodesign = rcodesign(&tools).unwrap();

    let app = bundle(&cfg, &bin).unwrap();
    let args = rcodesign_args(false, Some(&entitlements));
    rsign(&rcodesign, &app, None, &args).unwrap();
    check(&app);

    let app = bundle(&cfg, &bin).unwrap();
    let id = certificate(&tmp, &p12(&tmp, "pw", true), "pw").unwrap();
    rsign(&rcodesign, &app, Some(&id), &args).unwrap();
    check(&app);

    // A release's certificate is Apple's: rcodesign refuses this one for notarization.
    let notarized = rcodesign_args(true, Some(&entitlements));
    let e = rsign(&rcodesign, &app, Some(&id), &notarized).unwrap_err().to_string();
    assert!(e.contains("--for-notarization requires use of a Developer ID signing certificate"));

    let dir = Temp::new().unwrap();
    let id = certificate(&dir, &p12(&dir, "pw", false), "pw").unwrap();
    let e = rsign(&rcodesign, &app, Some(&id), &args).unwrap_err().to_string();
    assert!(e.ends_with("export APPLE_CERTIFICATE again with `openssl pkcs12 -export -legacy`"));
  }
}
