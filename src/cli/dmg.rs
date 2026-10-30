// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The macOS DMG: the app, a link to /Applications and the `.DS_Store` that lays out the window,
//! on an HFS+ volume named as the app, in a UDZO image; signed, and for a release notarized and
//! stapled.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use flate2::Crc;
use flate2::write::ZlibEncoder;

use crate::config::Config;
use crate::hfs::{self, Data, Entry, Kind};
use crate::macos::{self, Plist};
use crate::{Arch, bad, sign, tool};

/// `<Name>-<label>-macos-<arch>.dmg` in `dir` ([named](crate::out) by `tag`) of the signed bundle
/// `app`, signed as [crate::macos::app] signs it; notarized and stapled when it is a Developer ID's
/// and `notarize`, which a release (a `tag`) takes. Gives its path.
pub fn dmg(
  cfg: &Config,
  app: &Path,
  arch: Arch,
  tag: Option<&str>,
  notarize: bool,
  dir: &Path,
) -> io::Result<PathBuf> {
  let out = write(cfg, app, arch, tag, dir)?;
  sign::kept(out, |out| {
    let (signer, developer) = sign::sign_image(cfg, out, tag)?;
    if tag.is_some() && !developer {
      return Err(bad("a release is notarized, signed by a Developer ID"));
    }

    let notarized = notarize && developer;
    if notarized {
      sign::notary(cfg, out)?;
    }

    let state = if notarized { ", notarized" } else { "" };
    eprintln!("shipo: {} signed by {signer}{state}", out.display());
    Ok(())
  })
  .map(|(out, ())| out)
}

/// `<Name>-<label>-macos-<arch>.dmg` in `dir` of the bundle `app`, unsigned, [placed](tool::place)
/// over one there, so a failure leaves none.
fn write(
  cfg: &Config,
  app: &Path,
  arch: Arch,
  tag: Option<&str>,
  dir: &Path,
) -> io::Result<PathBuf> {
  let out = crate::out(dir, &cfg.name, tag, &format!("macos-{arch}.dmg"));
  tool::place(&out, |part| {
    let mut file = io::BufWriter::new(fs::File::create_new(part)?);
    image(&cfg.name, app, &mut file)?;
    file.into_inner().map_err(|e| e.into_error())?.sync_all()
  })?;
  Ok(out)
}

/// Writes to `out` the UDZO image of the volume `name` holding the bundle `app` as it is on disk,
/// as [Entry::read] gives it, the link and the `.DS_Store`; these two of the bundle's time.
fn image(name: &str, app: &Path, out: &mut impl io::Write) -> io::Result<()> {
  let app = Entry::read(app)?;
  if !matches!(app.kind, Kind::Dir(_)) {
    return Err(bad(format!("{}: not a bundle", app.name)));
  }

  // Finder finds a file's place by its name as the volume stores it.
  let layout = ds_store(&hfs::nfd(&app.name)?)?;
  let entry = |name: &str, mode, kind| Entry { name: name.into(), mode, time: app.time, kind };

  let link = entry("Applications", 0o755, Kind::Link("/Applications".into()));
  let store = entry(".DS_Store", 0o644, Kind::File(Data::Bytes(layout)));
  let root = entry(name, 0o755, Kind::Dir(vec![]));
  let root = Entry { kind: Kind::Dir(vec![app, link, store]), ..root };
  udzo(&hfs::volume(&root)?, "whole disk (Apple_HFS : 0)", out)
}

// MARK: UDIF

/// The sector, and a chunk: 1 MiB, of 2048 sectors.
const SECTOR: usize = 512;
const CHUNK: usize = 2048;

/// The chunk types of a `blkx` table.
const ZERO: u32 = 0;
const RAW: u32 = 1;
const ZLIB: u32 = 0x8000_0005;
const END: u32 = 0xFFFF_FFFF;

/// The checksum type of a CRC-32.
const CRC32: u32 = 2;

/// Writes the UDZO image of `disk`, whose length is whole sectors, to `out`, a chunk at a time,
/// the partition named `name` in its table, as `Apple_HFS` for an HFS+ volume.
fn udzo(disk: &[u8], name: &str, out: &mut impl Write) -> io::Result<()> {
  let sectors = (disk.len() / SECTOR) as u64;
  let (mut disk_crc, mut data_crc) = (Crc::new(), Crc::new());
  // The segment's id, of the data fork.
  let mut id = hmac_sha256::Hash::new();
  let mut packed = Vec::with_capacity(CHUNK * SECTOR);
  let mut chunks = vec![];
  let mut offset = 0u64;
  for (i, chunk) in disk.chunks(CHUNK * SECTOR).enumerate() {
    let (start, count) = ((i * CHUNK) as u64, (chunk.len() / SECTOR) as u64);
    disk_crc.update(chunk);
    let (kind, data) = match chunk.iter().all(|&b| b == 0) {
      true => (ZERO, &[][..]),
      false => {
        packed.clear();
        let mut z = ZlibEncoder::new(&mut packed, flate2::Compression::default());
        z.write_all(chunk)?;
        z.finish()?;
        match packed.len() < chunk.len() {
          true => (ZLIB, &packed[..]),
          false => (RAW, chunk),
        }
      }
    };

    out.write_all(data)?;
    data_crc.update(data);
    id.update(data);
    chunks.push((kind, start, count, offset, data.len() as u64));
    offset += data.len() as u64;
  }

  chunks.push((END, sectors, 0, offset, 0));
  let mut blkx = vec![];
  blkx.extend_from_slice(b"mish");
  push(&mut blkx, &[1]);
  push64(&mut blkx, &[0, sectors, 0]);
  // The buffer a chunk needs, in sectors, then the partition's number: one, 0.
  push(&mut blkx, &[CHUNK as u32 + 8, 0]);
  blkx.extend_from_slice(&[0; 24]);
  blkx.extend_from_slice(&checksum(disk_crc.sum()));
  push(&mut blkx, &[chunks.len() as u32]);
  for (kind, start, count, offset, length) in &chunks {
    push(&mut blkx, &[*kind, 0]);
    push64(&mut blkx, &[*start, *count, *offset, *length]);
  }

  let plist = plist(&blkx, name)?;
  out.write_all(&plist)?;

  let mut koly = vec![];
  koly.extend_from_slice(b"koly");
  // Version 4, the trailer's size, flattened.
  push(&mut koly, &[4, 512, 1]);
  // The data fork's running and own offset and its length; no resource fork.
  push64(&mut koly, &[0, 0, offset, 0, 0]);
  // Segment 1 of 1, and its id.
  push(&mut koly, &[1, 1]);
  koly.extend_from_slice(&id.finalize()[..16]);
  koly.extend_from_slice(&checksum(data_crc.sum()));
  push64(&mut koly, &[offset, plist.len() as u64]);
  koly.extend_from_slice(&[0; 120]);
  koly.extend_from_slice(&checksum(crc(&disk_crc.sum().to_be_bytes())));
  // A device image, its sectors.
  push(&mut koly, &[1]);
  push64(&mut koly, &[sectors]);
  koly.resize(512, 0);
  out.write_all(&koly)
}

/// The property list of the one `blkx` table, its partition named `name`.
fn plist(blkx: &[u8], name: &str) -> io::Result<Vec<u8>> {
  let table = macos::dict([
    ("Attributes", "0x0050".into()),
    ("CFName", name.into()),
    ("Data", Plist::Data(blkx.to_vec())),
    ("ID", "-1".into()),
    ("Name", name.into()),
  ]);

  let fork = macos::dict([("blkx", Plist::Array(vec![table]))]);
  macos::xml(&macos::dict([("resource-fork", fork)]))
}

/// A `UDIFChecksum` of a CRC-32: its type, its 32 bits, the value in the first of 32 words.
fn checksum(crc: u32) -> Vec<u8> {
  let mut out = vec![];
  push(&mut out, &[CRC32, 32, crc]);
  out.resize(136, 0);
  out
}

fn crc(data: &[u8]) -> u32 {
  let mut crc = Crc::new();
  crc.update(data);
  crc.sum()
}

fn push(out: &mut Vec<u8>, words: &[u32]) {
  words.iter().for_each(|w| out.extend_from_slice(&w.to_be_bytes()));
}

fn push64(out: &mut Vec<u8>, words: &[u64]) {
  words.iter().for_each(|w| out.extend_from_slice(&w.to_be_bytes()));
}

// MARK: .DS_Store

/// The allocator header's 16 bytes after its root block's offset, size and offset again: their
/// meaning is unknown, their value Finder's.
const HEAD: [u8; 16] = [0, 0, 0x10, 0x0C, 0, 0, 0, 0x87, 0, 0, 0x20, 0x0B, 0, 0, 0, 0];

/// The `.DS_Store` of a volume holding the bundle `app` (its file name) and `Applications`: a
/// 640×400 window with no toolbar, sidebar or background, both as 128 px icons, the app at
/// (160, 170) and Applications at (480, 170).
///
/// A "Bud1" buddy allocator over a 2 GiB space, 4 bytes into the file: its header at 0, block 0
/// (the block addresses, `DSDB` in its table of contents, the free blocks by size) at 2048, block 1
/// (the B-tree's header) at 32 and block 2 (its one leaf) at 4096. A block's address is its offset
/// or'd with the log2 of its size.
fn ds_store(app: &str) -> io::Result<Vec<u8>> {
  let window = macos::dict([
    ("WindowBounds", "{{200, 120}, {640, 400}}".into()),
    ("SidebarWidth", Plist::from(180)),
    ("ContainerShowSidebar", false.into()),
    ("PreviewPaneVisibility", false.into()),
    ("ShowToolbar", false.into()),
    ("ShowSidebar", false.into()),
    ("ShowStatusBar", false.into()),
    ("ShowPathbar", false.into()),
    ("ShowTabView", false.into()),
  ]);

  let view = macos::dict([
    ("viewOptionsVersion", Plist::from(1)),
    ("backgroundType", Plist::from(0)),
    ("backgroundColorRed", 1.0.into()),
    ("backgroundColorGreen", 1.0.into()),
    ("backgroundColorBlue", 1.0.into()),
    ("arrangeBy", "none".into()),
    ("gridSpacing", 100.0.into()),
    ("gridOffsetX", 0.0.into()),
    ("gridOffsetY", 0.0.into()),
    ("scrollPositionX", 0.0.into()),
    ("scrollPositionY", 0.0.into()),
    ("iconSize", 128.0.into()),
    ("textSize", 13.0.into()),
    ("labelOnBottom", true.into()),
    ("showIconPreview", true.into()),
    ("showItemInfo", false.into()),
  ]);

  // An icon's place, then 6 bytes of 0xFF and 2 of 0, as Finder writes them.
  let at = |x: u32, y: u32| [x, y, 0xFFFF_FFFF, 0xFFFF_0000].map(u32::to_be_bytes).concat();
  let mut records: Vec<Record> = vec![
    (".", b"bwsp", b"blob", blob(&macos::binary(&window)?)),
    (".", b"icvl", b"type", b"icnv".to_vec()),
    (".", b"icvp", b"blob", blob(&macos::binary(&view)?)),
    (".", b"vSrn", b"long", 1u32.to_be_bytes().to_vec()),
    (app, b"Iloc", b"blob", blob(&at(160, 170))),
    ("Applications", b"Iloc", b"blob", blob(&at(480, 170))),
  ];

  // Finder looks a name up in the sorted records: by the name in lower case, then the code.
  records.sort_by_key(|&(name, code, ..)| {
    (name.to_lowercase().encode_utf16().collect::<Vec<_>>(), *code)
  });

  let mut leaf = vec![];
  // No child pointer (a leaf), and the count.
  push(&mut leaf, &[0, records.len() as u32]);
  for (name, code, kind, data) in &records {
    let name: Vec<u16> = name.encode_utf16().collect();
    push(&mut leaf, &[name.len() as u32]);
    name.iter().for_each(|u| leaf.extend_from_slice(&u.to_be_bytes()));
    leaf.extend_from_slice(*code);
    leaf.extend_from_slice(*kind);
    leaf.extend_from_slice(data);
  }

  if leaf.len() > 4096 {
    return Err(bad(format!("{app}: too long a name for the .DS_Store's one leaf")));
  }

  let mut root = vec![];
  // The block count, a zero, the 256 address slots.
  let mut addresses = [0u32; 256];
  addresses[..3].copy_from_slice(&[2048 | 11, 32 | 5, 4096 | 12]);
  push(&mut root, &[3, 0]);
  push(&mut root, &addresses);
  // One directory entry: `DSDB` is block 1.
  push(&mut root, &[1]);
  root.push(4);
  root.extend_from_slice(b"DSDB");
  push(&mut root, &[1]);
  // The free blocks by log2 size: each one of 64 B to 1 GiB, at its own size's offset, but those
  // of 2048 and 4096, which blocks 0 and 2 take; 0 to 64 are the header and block 1.
  for width in 0..32 {
    match width {
      6..=30 if width != 11 && width != 12 => push(&mut root, &[1, 1 << width]),
      _ => push(&mut root, &[0]),
    }
  }

  let mut space = vec![0; 8192];
  space[..4].copy_from_slice(b"Bud1");
  space[4..16].copy_from_slice(&[2048u32, 2048, 2048].map(u32::to_be_bytes).concat());
  space[16..32].copy_from_slice(&HEAD);
  // The B-tree: its root node (block 2), no levels above the leaf, the records, one node, the
  // page size.
  let tree = [2u32, 0, records.len() as u32, 1, 4096].map(u32::to_be_bytes).concat();
  space[32..52].copy_from_slice(&tree);
  space[2048..2048 + root.len()].copy_from_slice(&root);
  space[4096..4096 + leaf.len()].copy_from_slice(&leaf);

  let mut out = 1u32.to_be_bytes().to_vec();
  out.extend_from_slice(&space);
  Ok(out)
}

/// A record: the file it is of, its code, its type and its data.
type Record<'a> = (&'a str, &'a [u8; 4], &'a [u8; 4], Vec<u8>);

/// `data` after its length.
fn blob(data: &[u8]) -> Vec<u8> {
  let mut out = (data.len() as u32).to_be_bytes().to_vec();
  out.extend_from_slice(data);
  out
}

#[cfg(test)]
pub(crate) mod tests {
  use std::env;
  #[cfg(unix)]
  use std::process::Command;

  use super::*;
  #[cfg(unix)]
  use crate::hfs::tests::bundle;
  #[cfg(target_os = "macos")]
  use crate::hfs::tests::{Attached, same};
  use crate::sign::NOTARY;
  use crate::tool::Temp;
  #[cfg(unix)]
  use crate::tool::run;

  /// A bundle `Hello.app` in `dir` of this test's binary, signed ad hoc by codesign, as `app` signs
  /// with no identity: Gatekeeper's checks run on a real signature.
  #[cfg(target_os = "macos")]
  fn signed_app(dir: &Path, name: &str) -> PathBuf {
    let app = dir.join(name);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::copy(env::current_exe().unwrap(), app.join("Contents/MacOS/hello")).unwrap();
    let info = macos::dict([
      ("CFBundleExecutable", "hello".into()),
      ("CFBundleIdentifier", "com.example.hello".into()),
      ("CFBundlePackageType", "APPL".into()),
    ]);
    fs::write(app.join("Contents/Info.plist"), macos::xml(&info).unwrap()).unwrap();
    let mut sign = Command::new("codesign");
    run(sign.args(["--force", "--sign", "-", "--options", "runtime"]).arg(&app)).unwrap();
    app
  }

  /// A test bundle's image: hdiutil verifies and reads it, fsck_hfs finds no fault, and mounted
  /// it holds the bundle as it is, the link and the `.DS_Store`: one top-level `.app`, which an
  /// older app's update takes.
  #[cfg(target_os = "macos")]
  #[test]
  fn compat_read_by_hdiutil() {
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
    let dmg = write(&cfg, &app, Arch::Aarch64, None, &tmp).unwrap();
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
    let dmg = write(&cfg, &app, Arch::X86_64, None, &tmp).unwrap();
    let exe = env::current_exe().unwrap();
    let rcodesign = sign::rcodesign(&exe.ancestors().nth(3).unwrap().join("tools")).unwrap();
    let check = |dmg: &Path| {
      if cfg!(target_os = "macos") {
        run(Command::new("codesign").args(["--verify", "--strict"]).arg(dmg)).unwrap();
        run(Command::new("hdiutil").args(["verify", "-quiet"]).arg(dmg)).unwrap();
      }
    };

    sign::rsign(&rcodesign, &dmg, None, &[]).unwrap();
    check(&dmg);

    let dmg = write(&cfg, &app, Arch::X86_64, None, &tmp).unwrap();
    let p12 = crate::sign::tests::p12(&tmp, "pw", true);
    let p12 = sign::certificate(&tmp, &p12, "pw").unwrap();
    sign::rsign(&rcodesign, &dmg, Some(&p12), &[]).unwrap();
    check(&dmg);
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

  /// A big-endian word of `data` at `at`.
  pub fn word(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(crate::tool::get(data, at).unwrap())
  }

  /// A record as the leaf holds it: the name, code, type and data.
  type Parsed = (String, [u8; 4], [u8; 4], Vec<u8>);

  /// Reads a `.DS_Store` as Finder does, checking its allocator: the header, the blocks and the
  /// free lists tile the 2 GiB space with buddies, each at an offset of its own size. Gives the
  /// records of its B-tree, which has one leaf.
  pub fn parse(file: &[u8]) -> Vec<Parsed> {
    assert_eq!(word(file, 0), 1);
    let space = &file[4..];
    assert_eq!(&space[..4], b"Bud1");
    let (root, size) = (word(space, 4) as usize, word(space, 8) as usize);
    assert_eq!(word(space, 12) as usize, root);

    // Block 0, the allocator's own: the addresses, the table of contents, the free lists.
    let count = word(space, root) as usize;
    let addresses: Vec<u32> = (0..count).map(|i| word(space, root + 8 + 4 * i)).collect();
    let block = |n: usize| {
      let a = addresses[n];
      ((a & !0x1F) as usize, 1usize << (a & 0x1F))
    };

    assert_eq!(block(0), (root, size));
    let mut at = root + 8 + 4 * 256;
    let entries = word(space, at);
    at += 4;
    let mut dsdb = None;
    for _ in 0..entries {
      let len = space[at] as usize;
      let name = &space[at + 1..at + 1 + len];
      if name == b"DSDB" {
        dsdb = Some(word(space, at + 1 + len) as usize);
      }

      at += 1 + len + 4;
    }

    // The header's 32 bytes, the blocks and the free blocks, by offset and size.
    let mut used = vec![(0usize, 32usize)];
    used.extend((0..count).map(block));
    for width in 0..32 {
      let n = word(space, at);
      at += 4;
      for _ in 0..n {
        used.push((word(space, at) as usize, 1 << width));
        at += 4;
      }
    }

    assert!(at <= root + size);
    used.sort();
    let mut end = 0;
    for &(offset, size) in &used {
      assert_eq!(offset, end, "a gap or an overlap at {offset}");
      assert_eq!(offset % size, 0, "a block of {size} at {offset}");
      end = offset + size;
    }

    assert_eq!(end, 1 << 31);

    // The B-tree's header, then its one node.
    let (tree, _) = block(dsdb.unwrap());
    let (node, levels, records, nodes, page) = (
      word(space, tree),
      word(space, tree + 4),
      word(space, tree + 8),
      word(space, tree + 12),
      word(space, tree + 16),
    );
    assert_eq!((levels, nodes, page), (0, 1, 4096));
    let (mut at, end) = block(node as usize);
    let end = at + end;
    assert_eq!(word(space, at), 0, "a leaf");
    assert_eq!(word(space, at + 4), records);
    at += 8;

    let mut out = vec![];
    for _ in 0..records {
      let len = word(space, at) as usize;
      let units: Vec<u16> = space[at + 4..at + 4 + 2 * len]
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
      at += 4 + 2 * len;
      let (code, kind) =
        (space[at..at + 4].try_into().unwrap(), space[at + 4..at + 8].try_into().unwrap());
      at += 8;
      let size = match &kind {
        b"blob" => 4 + word(space, at) as usize,
        b"long" | b"type" => 4,
        _ => panic!("{kind:?}"),
      };

      let data = space[at..at + size].to_vec();
      at += size;
      out.push((String::from_utf16(&units).unwrap(), code, kind, data));
    }

    assert!(at <= end);
    out
  }

  #[test]
  fn ds_store_layout() {
    let file = ds_store("Hello World.app").unwrap();
    assert_eq!(file.len(), 8196);
    let records = parse(&file);
    let keys: Vec<_> = records.iter().map(|(n, c, k, _)| (n.as_str(), c, k)).collect();
    assert_eq!(
      keys,
      [
        (".", b"bwsp", b"blob"),
        (".", b"icvl", b"type"),
        (".", b"icvp", b"blob"),
        (".", b"vSrn", b"long"),
        ("Applications", b"Iloc", b"blob"),
        ("Hello World.app", b"Iloc", b"blob"),
      ]
    );

    let place = |data: &[u8]| (word(data, 4), word(data, 8), data[12..20].to_vec());
    let rest = vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0];
    assert_eq!(place(&records[4].3), (480, 170, rest.clone()));
    assert_eq!(place(&records[5].3), (160, 170, rest));
    assert_eq!(records[1].3, b"icnv");
    assert_eq!(records[3].3, [0, 0, 0, 1]);
    assert_eq!(&records[0].3[4..12], b"bplist00");

    // In lower case "applications" comes after "a.app".
    let names: Vec<_> = parse(&ds_store("a.app").unwrap()).into_iter().map(|r| r.0).collect();
    assert_eq!(names[4..], ["a.app", "Applications"]);

    let e = ds_store(&"x".repeat(2000)).unwrap_err().to_string();
    assert!(e.ends_with("too long a name for the .DS_Store's one leaf"));
  }

  /// The window's and the view's plists, read back by Python's `plistlib`.
  #[cfg(target_os = "macos")]
  #[test]
  fn plists() {
    use std::process::Command;

    let records = parse(&ds_store("Hello.app").unwrap());
    let read = |data: &[u8]| {
      let hex = crate::tool::hex(&data[4..]);
      let code = format!(
        "import plistlib\nd = plistlib.loads(bytes.fromhex('{hex}'))\nprint(sorted(d.items()))"
      );
      let out = Command::new("python3").args(["-I", "-c", &code]).output().unwrap();
      assert!(out.status.success());
      String::from_utf8(out.stdout).unwrap()
    };

    let window = concat!(
      "[('ContainerShowSidebar', False), ('PreviewPaneVisibility', False), ('ShowPathbar', False), ",
      "('ShowSidebar', False), ('ShowStatusBar', False), ('ShowTabView', False), ",
      "('ShowToolbar', False), ('SidebarWidth', 180), ('WindowBounds', '{{200, 120}, {640, 400}}')]\n",
    );

    assert_eq!(read(&records[0].3), window);
    let view = read(&records[2].3);
    for item in [
      "('iconSize', 128.0)",
      "('arrangeBy', 'none')",
      "('gridSpacing', 100.0)",
      "('labelOnBottom', True)",
    ] {
      assert!(view.contains(item), "{view}");
    }
  }
}
