// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The macOS DMG: the app, a link to /Applications and the `.DS_Store` that lays out the window,
//! on an HFS+ volume named as the app, in a UDZO image; signed, and for a release notarized and
//! stapled.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, fs, io};

use crate::config::Config;
use crate::ds_store::ds_store;
use crate::hfs::{self, Data, Entry, Kind};
use crate::macos::{self, rcodesign};
use crate::tool::{Temp, private, run, run_until};
use crate::{bad, udif};

/// The secrets of the App Store Connect API key that notarizes: the `.p8`, its id and the issuer.
pub const NOTARY: [&str; 3] = ["APPLE_API_KEY", "APPLE_API_KEY_ID", "APPLE_API_ISSUER"];

/// `<Name>-dev-macos-<arch>.dmg` in `dir` of the signed bundle `app`, signed as [macos::app] signs
/// it; notarized and stapled when it is a Developer ID's and `notarize`, which a release (a `tag`)
/// takes. Gives its path.
pub fn dmg(
  cfg: &Config,
  app: &Path,
  arch: &str,
  tag: Option<&str>,
  notarize: bool,
  dir: &Path,
) -> io::Result<PathBuf> {
  let out = write(cfg, app, arch, dir)?;
  kept(out, |out| {
    let (signer, developer) = macos::sign_image(cfg, out, tag)?;
    if tag.is_some() && !developer {
      return Err(bad("a release is notarized, signed by a Developer ID"));
    }

    let notarized = notarize && developer;
    if notarized {
      notary(cfg, out)?;
    }

    let state = if notarized { ", notarized" } else { "" };
    eprintln!("shipo: {} signed by {signer}{state}", out.display());
    Ok(())
  })
}

/// `dmg` once `finish` signed it, or notarized it too; removed when that fails, so no unsigned
/// image is left among a release's files.
fn kept(dmg: PathBuf, finish: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<PathBuf> {
  match finish(&dmg) {
    Ok(()) => Ok(dmg),
    Err(e) => {
      let _ = fs::remove_file(&dmg);
      Err(io::Error::new(e.kind(), format!("{}: {e}; no image", dmg.display())))
    }
  }
}

/// `<Name>-dev-macos-<arch>.dmg` in `dir` of the bundle `app`, unsigned, in place of one there:
/// written beside it first, so a failure leaves none.
pub fn write(cfg: &Config, app: &Path, arch: &str, dir: &Path) -> io::Result<PathBuf> {
  let out = dir.join(format!("{}-dev-macos-{arch}.dmg", cfg.name));
  let part = dir.join(format!(".{}-dev-macos-{arch}.dmg.part", cfg.name));
  let written = fs::File::create(&part).and_then(|file| {
    let mut file = io::BufWriter::new(file);
    image(&cfg.name, app, &mut file)?;
    file.into_inner().map_err(|e| e.into_error())?.sync_all()
  });

  if let Err(e) = written.and_then(|()| fs::rename(&part, &out)) {
    let _ = fs::remove_file(&part);
    return Err(e);
  }

  Ok(out)
}

/// Submits `dmg` to Apple's notary service by rcodesign with the API key of [NOTARY], waits up to
/// 20 minutes and staples the ticket. The key goes through files only its owner reads.
fn notary(cfg: &Config, dmg: &Path) -> io::Result<()> {
  let rcodesign = rcodesign(&cfg.target.join("tools"))?;
  let var = |name| env::var(name).map_err(|e| bad(format!("{name}: {e}")));
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

/// Writes to `out` the UDZO image of the volume `name` holding the bundle `app` as it is on disk,
/// the link and the `.DS_Store`; these two of the bundle's owners and time.
pub fn image(name: &str, app: &Path, out: &mut impl io::Write) -> io::Result<()> {
  let app = Entry::read(app)?;
  if !matches!(app.kind, Kind::Dir(_)) {
    return Err(bad(format!("{}: not a bundle", app.name)));
  }

  // Finder finds a file's place by its name as the volume stores it.
  let layout = ds_store(&hfs::nfd(&app.name)?)?;
  let entry = |name: &str, mode, kind| Entry {
    name: name.into(),
    mode,
    uid: app.uid,
    gid: app.gid,
    time: app.time,
    kind,
  };

  let link = entry("Applications", 0o755, Kind::Link("/Applications".into()));
  let store = entry(".DS_Store", 0o644, Kind::File(Data::Bytes(layout)));
  let root = entry(name, 0o755, Kind::Dir(vec![]));
  let root = Entry { kind: Kind::Dir(vec![app, link, store]), ..root };
  udif::udzo(&hfs::volume(&root)?, "whole disk (Apple_HFS : 0)", out)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ds_store::tests::{parse, word};
  #[cfg(unix)]
  use crate::hfs::tests::bundle;
  #[cfg(target_os = "macos")]
  use crate::hfs::tests::{Attached, same};
  #[cfg(target_os = "macos")]
  use crate::plist::Plist;

  /// A bundle `Hello.app` in `dir` of this test's binary, signed ad hoc by codesign, as `app` signs
  /// with no identity: Gatekeeper's checks run on a real signature.
  #[cfg(target_os = "macos")]
  fn signed_app(dir: &Path, name: &str) -> PathBuf {
    let app = dir.join(name);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::copy(env::current_exe().unwrap(), app.join("Contents/MacOS/hello")).unwrap();
    let info = Plist::dict([
      ("CFBundleExecutable", "hello".into()),
      ("CFBundleIdentifier", "com.example.hello".into()),
      ("CFBundlePackageType", "APPL".into()),
    ]);
    fs::write(app.join("Contents/Info.plist"), info.xml().unwrap()).unwrap();
    let mut sign = Command::new("codesign");
    run(sign.args(["--force", "--sign", "-", "--options", "runtime"]).arg(&app)).unwrap();
    app
  }

  /// A test bundle's image: hdiutil verifies and reads it, fsck_hfs finds no fault, and mounted
  /// it holds the bundle as it is, the link and the `.DS_Store`.
  #[cfg(target_os = "macos")]
  #[test]
  fn read_by_hdiutil() {
    let tmp = Temp::new().unwrap();
    let app = bundle(&tmp, "Wavö Test.app");
    let dmg = tmp.join("test.dmg");
    image("Wavö Test", &app, &mut fs::File::create(&dmg).unwrap()).unwrap();

    run(Command::new("hdiutil").args(["verify", "-quiet"]).arg(&dmg)).unwrap();
    let info = run(Command::new("hdiutil").args(["imageinfo"]).arg(&dmg)).unwrap();
    let info = String::from_utf8(info).unwrap();
    assert!(info.contains("Format: UDZO"), "{info}");
    let said = Attached::new(&dmg, false, None).fsck();
    assert!(said.contains("The volume name is Wavo\u{308} Test"), "{said}");
    assert!(said.contains("appears to be OK"), "{said}");

    let mount = tmp.join("mnt");
    fs::create_dir(&mount).unwrap();
    let attached = Attached::new(&dmg, false, Some(&mount));
    let names =
      fs::read_dir(&mount).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap());
    let mut names: Vec<_> = names.collect();
    names.sort();
    assert_eq!(names, [".DS_Store", "Applications", "Wavo\u{308} Test.app"]);
    same(&Entry::read(&app).unwrap(), &mount.join("Wavo\u{308} Test.app"));
    assert_eq!(fs::read_link(mount.join("Applications")).unwrap(), Path::new("/Applications"));
    drop(attached);
  }

  /// `release`'s DMG of an app signed ad hoc, then the DMG signed ad hoc by codesign: both
  /// signatures verify, the image still does, and the mounted `.DS_Store` lays out the window.
  #[cfg(target_os = "macos")]
  #[test]
  fn signed_by_codesign() {
    let tmp = Temp::new().unwrap();
    let mut cfg = crate::macos::tests::config(&tmp, "");
    cfg.name = "Wavö".into();
    let app = signed_app(&tmp, "Wavö.app");
    let dmg = write(&cfg, &app, "aarch64", &tmp).unwrap();
    assert_eq!(dmg, tmp.join("Wavö-dev-macos-aarch64.dmg"));

    run(Command::new("codesign").args(["--force", "--sign", "-"]).arg(&dmg)).unwrap();
    run(Command::new("codesign").args(["--verify", "--strict"]).arg(&dmg)).unwrap();
    run(Command::new("hdiutil").args(["verify", "-quiet"]).arg(&dmg)).unwrap();

    let mount = tmp.join("mnt");
    fs::create_dir(&mount).unwrap();
    let attached = Attached::new(&dmg, false, Some(&mount));
    let bundle = mount.join("Wavo\u{308}.app");
    run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&bundle)).unwrap();
    layout(&fs::read(mount.join(".DS_Store")).unwrap(), "Wavo\u{308}.app");
    drop(attached);
  }

  /// Checks the `.DS_Store` `file` lays out the window for the bundle `app`: 640×400, 128 px
  /// icons, the app at (160, 170) and Applications at (480, 170).
  fn layout(file: &[u8], app: &str) {
    let records = parse(file);
    let find = |name: &str, code: &[u8; 4]| {
      let found = records.iter().find(|r| r.0 == name && &r.1 == code);
      found.unwrap_or_else(|| panic!("no {name} {code:?}")).3.clone()
    };

    let place = |data: &[u8]| (word(data, 4), word(data, 8));
    assert_eq!(place(&find(app, b"Iloc")), (160, 170));
    assert_eq!(place(&find("Applications", b"Iloc")), (480, 170));
    let window = find(".", b"bwsp");
    let bounds = b"{{200, 120}, {640, 400}}";
    assert!(window.windows(bounds.len()).any(|w| w == bounds));
    // `iconSize` is a real of 128 in the view's binary plist: 0x23, then 128.0 as a double.
    let view = find(".", b"icvp");
    let size = [&[0x23][..], &128f64.to_be_bytes()].concat();
    assert!(view.windows(size.len()).any(|w| w == size), "{view:?}");
  }

  /// rcodesign, as the packer fetches it, signs a DMG ad hoc and by a throwaway certificate's
  /// `.p12`; on a Mac codesign verifies both.
  #[cfg(unix)]
  #[test]
  #[ignore = "fetches rcodesign from GitHub"]
  fn signed_by_rcodesign() {
    let tmp = Temp::new().unwrap();
    let cfg = crate::macos::tests::config(&tmp, "");
    let app = bundle(&tmp, "Hello.app");
    let dmg = write(&cfg, &app, "x86_64", &tmp).unwrap();
    let exe = env::current_exe().unwrap();
    let rcodesign = rcodesign(&exe.ancestors().nth(3).unwrap().join("tools")).unwrap();
    let check = |dmg: &Path| {
      if cfg!(target_os = "macos") {
        run(Command::new("codesign").args(["--verify", "--strict"]).arg(dmg)).unwrap();
        run(Command::new("hdiutil").args(["verify", "-quiet"]).arg(dmg)).unwrap();
      }
    };

    macos::rsign(&rcodesign, &dmg, None, &[]).unwrap();
    check(&dmg);

    let dmg = write(&cfg, &app, "x86_64", &tmp).unwrap();
    let p12 = crate::macos::tests::p12(&tmp, "pw", true);
    let p12 = macos::certificate(&tmp, &p12, "pw").unwrap();
    macos::rsign(&rcodesign, &dmg, Some(&p12), &[]).unwrap();
    check(&dmg);
  }

  /// An image whose signature or notarization fails is removed; one signed stays.
  #[test]
  fn removed_unsigned() {
    let tmp = Temp::new().unwrap();
    let dmg = tmp.join("Hello-dev-macos-aarch64.dmg");
    fs::write(&dmg, b"image").unwrap();
    assert_eq!(kept(dmg.clone(), |_| Ok(())).unwrap(), dmg);
    assert!(dmg.exists());

    let e = kept(dmg.clone(), |_| Err(io::Error::other("notarizing: rejected"))).unwrap_err();
    assert_eq!(e.to_string(), format!("{}: notarizing: rejected; no image", dmg.display()));
    assert!(!dmg.exists());
  }

  /// A release of a macOS binary without the notary's secrets fails before the app is made.
  #[test]
  fn release_needs_notary() {
    if NOTARY.iter().any(|n| env::var_os(n).is_some()) {
      return;
    }

    let tmp = Temp::new().unwrap();
    let mut cfg = crate::macos::tests::config(&tmp, "");
    cfg.version = "1.2.0".into();
    cfg.repository = "https://github.com/vladkens/hello".into();
    let key = tmp.join("update.key");
    crate::feed::keygen(std::slice::from_ref(&key)).unwrap();
    let dist = tmp.join("dist");
    fs::create_dir_all(dist.join("aarch64-apple-darwin")).unwrap();
    fs::write(dist.join("aarch64-apple-darwin/hello"), b"").unwrap();

    let e = crate::release(&cfg, &dist, Some("v1.2.0"), Some(&key)).unwrap_err().to_string();
    let want = "v1.2.0 is a release, signed and notarized: set APPLE_API_KEY, APPLE_API_KEY_ID, \
      APPLE_API_ISSUER";
    assert_eq!(e, want);
    assert!(!tmp.join("target").exists());
  }

  #[test]
  fn layout_of_names() {
    let app = hfs::nfd("Wavö.app").unwrap();
    layout(&ds_store(&app).unwrap(), "Wavo\u{308}.app");
  }
}
