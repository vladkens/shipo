// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Linux packages of the app's binary: its `.desktop` entry, the AppImage (the type2 runtime
//! with a squashfs of the AppDir appended) and the `.deb`, both built in memory.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write};
use std::iter;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use tar::{Builder, EntryType, Header};

use crate::config::Config;
use crate::image::{Image, icons};
use crate::tool::{self, read};
use crate::{Arch, at, bad};

/// The release of the AppImage type2 runtime: static, so the AppImage needs no libfuse2.
const RUNTIME: &str = "20251108";

/// The AppImage's icon's side in px: hicolor has no folder past 512.
const ICON: usize = 512;
/// The `.deb`'s icons' sides in px, each in its hicolor folder.
const HICOLOR: [usize; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

/// The characters an `Exec` argument holds only in quotes, by the desktop entry spec.
const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";

// MARK: Desktop entry

/// The `.desktop` entry, `<id>.desktop`: the app started by its binary's name on the PATH, its
/// windows matched to the entry by the app's id (X11's `WM_CLASS`, Wayland's app id), in
/// `linux.categories`, `Utility` if none.
fn desktop(cfg: &Config) -> io::Result<String> {
  let categories = match cfg.linux.categories.as_slice() {
    [] => &["Utility".to_string()][..],
    some => some,
  };

  let mut out = format!("[Desktop Entry]\nType=Application\nName={}\n", escape(&cfg.name));
  if !cfg.comment.is_empty() {
    let _ = writeln!(out, "Comment={}", escape(&cfg.comment));
  }

  let _ = writeln!(out, "Exec={}", escape(&exec(&cfg.bin)));
  let _ = writeln!(out, "Icon={}", escape(&cfg.id));
  let _ = writeln!(out, "StartupWMClass={}", escape(&cfg.id));
  out += "StartupNotify=true\nTerminal=false\n";
  let list: String = categories.iter().map(|c| escape(c).replace(';', "\\;") + ";").collect();
  let _ = writeln!(out, "Categories={list}");
  Ok(out)
}

/// `text` as a desktop entry's string value: `\\`, `\n`, `\t`, `\r`, and `\s` for a space at
/// either end, which a reader would trim.
fn escape(text: &str) -> String {
  let last = text.chars().count().saturating_sub(1);
  let mut out = String::new();
  for (i, c) in text.chars().enumerate() {
    match c {
      '\\' => out += "\\\\",
      '\n' => out += "\\n",
      '\t' => out += "\\t",
      '\r' => out += "\\r",
      ' ' if i == 0 || i == last => out += "\\s",
      c => out.push(c),
    }
  }

  out
}

/// `arg` as an argument of `Exec`, before [escape]: `%` doubled, not a field code; in double
/// quotes when it holds a [RESERVED] character, each `"`, `` ` ``, `$` and `\` in it after a `\`.
fn exec(arg: &str) -> String {
  let arg = arg.replace('%', "%%");
  if !arg.contains(|c| RESERVED.contains(c)) {
    return arg;
  }

  let quoted =
    arg.chars().flat_map(|c| iter::once('\\').filter(move |_| "\"`$\\".contains(c)).chain([c]));
  format!("\"{}\"", quoted.collect::<String>())
}

/// The icon's visible tile at each of `sizes`, PNGs by size: docks show the image as it is, so
/// the margin a macOS icon keeps would shrink it.
fn pngs(cfg: &Config, sizes: &[usize]) -> io::Result<BTreeMap<usize, Vec<u8>>> {
  let images = icons(&Image::read(&cfg.icon)?, sizes, true)?;
  images.into_iter().map(|(n, image)| Ok((n, image.encode()?))).collect()
}

// MARK: AppImage

/// `<Name>-dev-linux-<arch>.AppImage` in `out_dir` of the binary `bin`, 0755: the type2 runtime of
/// `arch`, fetched into `<target>/tools` once and pinned by SHA-256, with the squashfs of the
/// [appdir] appended. The runtime mounts it, by FUSE or unpacked, and sets `APPIMAGE` and `APPDIR`.
pub fn appimage(cfg: &Config, bin: &Path, arch: Arch, out_dir: &Path) -> io::Result<PathBuf> {
  // The runtime's SHA-256.
  let sha256 = match arch {
    Arch::X86_64 => "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d",
    Arch::Aarch64 => "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444",
  };

  let icon = pngs(cfg, &[ICON])?.remove(&ICON).unwrap_or_default();
  let url =
    format!("https://github.com/AppImage/type2-runtime/releases/download/{RUNTIME}/runtime-{arch}");
  let runtime = cfg.target.join("tools").join(format!("runtime-{RUNTIME}-{arch}"));
  let runtime = tool::fetch(&url, sha256, &runtime)?;

  let mtime = u32::try_from(mtime(bin)?).map_err(|_| bad("the squashfs's time is past 2106"))?;
  let squashfs = squashfs(&appdir(cfg, read(bin)?, icon)?, mtime)?;
  let out = out_dir.join(format!("{}-dev-linux-{arch}.AppImage", cfg.name));
  tool::place(&out, |part| {
    let mut file = File::create_new(part)?;
    io::copy(&mut File::open(&runtime)?, &mut file)?;
    io::Write::write_all(&mut file, &squashfs)?;
    file.sync_all()?;
    tool::set_mode(part, 0o755)
  })?;
  Ok(out)
}

/// The AppDir of the binary `bin` and the icon's PNG: `usr/bin/<bin>` (0755), `AppRun` a link to
/// it, `<id>.desktop`, `<id>.png` and `.DirIcon` a link to it.
fn appdir(cfg: &Config, bin: Vec<u8>, icon: Vec<u8>) -> io::Result<Tree> {
  let mut tree = Tree::default();
  tree.file(&format!("usr/bin/{}", cfg.bin), 0o755, bin);
  tree.file(&format!("{}.desktop", cfg.id), 0o644, desktop(cfg)?);
  tree.file(&format!("{}.png", cfg.id), 0o644, icon);
  tree.link("AppRun", &format!("usr/bin/{}", cfg.bin));
  tree.link(".DirIcon", &format!("{}.png", cfg.id));
  Ok(tree)
}

// MARK: Debian package

/// `<Name>-dev-linux-<arch>.deb` in `out_dir` of the binary `bin`: `/usr/bin/<bin>`, the
/// `.desktop` entry, the icon at each [HICOLOR] size and the [license] as the copyright. Its
/// version is a `dev` build's or a release's.
pub fn deb(cfg: &Config, bin: &Path, arch: Arch, dev: bool, out_dir: &Path) -> io::Result<PathBuf> {
  let pkg = package(cfg)?;
  let pngs = pngs(cfg, &HICOLOR)?;
  let license = license(cfg);
  if license.is_none() {
    eprintln!(
      "shipo: no LICENSE in {} or its workspace: the .deb has no copyright",
      cfg.root.display()
    );
  }

  let mut tree = Tree::default();
  tree.file(&format!("usr/bin/{}", cfg.bin), 0o755, read(bin)?);
  tree.file(&format!("usr/share/applications/{}.desktop", cfg.id), 0o644, desktop(cfg)?);
  for (n, png) in pngs {
    tree.file(&format!("usr/share/icons/hicolor/{n}x{n}/apps/{}.png", cfg.id), 0o644, png);
  }

  if let Some(license) = license {
    tree.file(&format!("usr/share/doc/{pkg}/copyright"), 0o644, read(&license)?);
  }

  let control = control(cfg, &pkg, arch, dev, tree.size())?;
  let data = deb_archive(&control, &tree, mtime(bin)?)?;
  let out = out_dir.join(format!("{}-dev-linux-{arch}.deb", cfg.name));
  tool::place(&out, |part| File::create_new(part)?.write_all(&data))?;
  Ok(out)
}

/// The `.deb`'s control file for the files of `size` bytes: [package] `pkg`, [deb_version] of a
/// dev build if `dev`, [maintainer], `linux.deb-depends` and `deb-recommends`, and the
/// [description]. A field holds one line; an empty one is left out.
fn control(cfg: &Config, pkg: &str, arch: Arch, dev: bool, size: u64) -> io::Result<String> {
  let fields = [
    ("Package", pkg.to_string()),
    ("Version", deb_version(&cfg.version, dev)),
    ("Architecture", arch.deb().into()),
    ("Maintainer", maintainer(cfg)?),
    ("Installed-Size", size.div_ceil(1024).to_string()),
    ("Depends", cfg.linux.deb_depends.join(", ")),
    ("Recommends", cfg.linux.deb_recommends.join(", ")),
    ("Section", "utils".into()),
    ("Priority", "optional".into()),
  ];

  let mut out = String::new();
  for (key, value) in fields {
    if value.contains(['\n', '\r']) {
      return Err(bad(format!("the .deb's {key} is one line, not {value:?}")));
    }

    if !value.is_empty() {
      let _ = writeln!(out, "{key}: {value}");
    }
  }

  Ok(out + &description(cfg))
}

/// The `Description` field of the comment, else the name: its first line the synopsis, the rest
/// the extended description, if any, each line after a space and an empty one ` .`.
fn description(cfg: &Config) -> String {
  let text = match cfg.comment.trim() {
    "" => cfg.name.trim(),
    comment => comment,
  };

  let mut lines = text.lines().map(|l| l.trim_end_matches('\r').trim_end());
  let synopsis = lines.next().unwrap_or_default();
  let rest: Vec<_> = lines.collect();
  let first = rest.iter().position(|l| !l.is_empty()).unwrap_or(rest.len());

  let mut out = format!("Description: {synopsis}\n");
  for line in &rest[first..] {
    match line.is_empty() {
      true => out += " .\n",
      false => out += &format!(" {line}\n"),
    }
  }

  out
}

/// `version`, Cargo's, as dpkg orders it: without its `+` build, the `-` before the pre-release
/// as `~`, so it sorts before its release, and each `-` in the pre-release as `.`, which dpkg would
/// take for a revision. A `dev` build gets `~dev` after, so it sorts before the version it builds.
fn deb_version(version: &str, dev: bool) -> String {
  let version = version.split('+').next().unwrap_or(version);
  let version = match version.split_once('-') {
    Some((core, pre)) => format!("{core}~{}", pre.replace('-', ".")),
    None => version.to_string(),
  };

  match dev {
    true => version + "~dev",
    false => version,
  }
}

/// The package's name: `linux.deb-package`, else the binary's in lower case with `_` as `-`, as
/// Debian allows one: `[a-z0-9][a-z0-9+.-]+`.
fn package(cfg: &Config) -> io::Result<String> {
  let name = match &cfg.linux.deb_package {
    None => cfg.bin.to_lowercase().replace('_', "-"),
    Some(name) => name.clone(),
  };

  let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
  let valid = name.len() >= 2
    && name.starts_with(alnum)
    && name.chars().all(|c| alnum(c) || "+-.".contains(c));
  match valid {
    true => Ok(name),
    false => Err(bad(format!(
      "{name:?} is no Debian package name, [a-z0-9][a-z0-9+.-]+: set linux.deb-package"
    ))),
  }
}

/// The `.deb`'s `Name <mail>`: the publisher, and the mail of the first author with one, as
/// Debian wants a maintainer.
fn maintainer(cfg: &Config) -> io::Result<String> {
  let mail = |author: &String| {
    let (_, mail) = author.split_once('<')?;
    let mail = mail.strip_suffix('>')?;
    mail.contains('@').then(|| mail.to_string())
  };

  match cfg.authors.iter().find_map(mail) {
    Some(mail) if !cfg.publisher.is_empty() => Ok(format!("{} <{mail}>", cfg.publisher)),
    _ => Err(bad("the .deb's Maintainer is `Name <mail>`: an author `Name <mail>` in Cargo.toml")),
  }
}

/// The `LICENSE` file in the package's folder, else in its workspace's; `None` if neither has one.
fn license(cfg: &Config) -> Option<PathBuf> {
  [&cfg.root, &cfg.workspace].into_iter().map(|dir| dir.join("LICENSE")).find(|p| p.is_file())
}

// MARK: Files

/// The time of the packages' files: `$SOURCE_DATE_EPOCH` when set, else the binary `bin`'s, so a
/// build of the same binary packs the same bytes.
fn mtime(bin: &Path) -> io::Result<u64> {
  if let Some(epoch) = std::env::var_os("SOURCE_DATE_EPOCH") {
    let epoch = epoch.to_string_lossy();
    return epoch
      .trim()
      .parse()
      .map_err(|_| bad(format!("SOURCE_DATE_EPOCH is {epoch:?}, not seconds")));
  }

  let time = fs::metadata(bin).and_then(|m| m.modified()).map_err(at(bin))?;
  Ok(time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

// MARK: .deb archive

/// The `.deb` of the control file `control` and the files `data`, all of the time `mtime`.
fn deb_archive(control: &str, data: &Tree, mtime: u64) -> io::Result<Vec<u8>> {
  let mut meta = Tree::default();
  meta.file("control", 0o644, control);

  let mut out = b"!<arch>\n".to_vec();
  member(&mut out, "debian-binary", b"2.0\n", mtime);
  member(&mut out, "control.tar.gz", &gzip(&archive(&meta, mtime)?)?, mtime);
  member(&mut out, "data.tar.gz", &gzip(&archive(data, mtime)?)?, mtime);
  Ok(out)
}

/// The tar archive of `tree` under `./`, `./` itself first, each entry owned by root and of the
/// time `mtime`. Headers are ustar; a name or a link target past their 100 bytes gets a GNU long
/// one before it. The crate would drop the `./` of a name, so the names are put in here.
fn archive(tree: &Tree, mtime: u64) -> io::Result<Vec<u8>> {
  let mut out = Builder::new(vec![]);
  let mut put = |name: &str, kind: EntryType, mode: u32, link: &str, data: &[u8]| {
    let mut head = Header::new_ustar();
    head.set_entry_type(kind);
    head.set_mode(mode & 0o7777);
    head.set_size(data.len() as u64);
    head.set_mtime(mtime);
    head.set_uid(0);
    head.set_gid(0);
    head.set_username("root")?;
    head.set_groupname("root")?;

    let (name, link) = (name.as_bytes(), link.as_bytes());
    let old = head.as_old_mut();
    for (text, slot, kind) in [
      (name, &mut old.name, EntryType::GNULongName),
      (link, &mut old.linkname, EntryType::GNULongLink),
    ] {
      if text.len() > slot.len() {
        long(&mut out, kind, text)?;
      }

      let n = text.len().min(slot.len());
      slot[..n].copy_from_slice(&text[..n]);
    }

    head.set_cksum();
    out.append(&head, data)
  };

  put("./", EntryType::Directory, 0o755, "", &[])?;
  for (path, mode, node) in tree.iter() {
    match node {
      Node::Dir => put(&format!("./{path}/"), EntryType::Directory, mode, "", &[])?,
      Node::Link(to) => put(&format!("./{path}"), EntryType::Symlink, mode, to, &[])?,
      Node::File(data) => put(&format!("./{path}"), EntryType::Regular, mode, "", data)?,
    }
  }

  out.into_inner()
}

/// The GNU entry of the type `kind`, a long name or link, holding `text` for the entry after it.
fn long(out: &mut Builder<Vec<u8>>, kind: EntryType, text: &[u8]) -> io::Result<()> {
  let mut head = Header::new_gnu();
  head.as_old_mut().name[..13].copy_from_slice(b"././@LongLink");
  head.set_entry_type(kind);
  head.set_mode(0o644);
  head.set_uid(0);
  head.set_gid(0);
  head.set_mtime(0);
  head.set_size(text.len() as u64 + 1);
  head.set_cksum();
  out.append(&head, [text, b"\0"].concat().as_slice())
}

/// The `ar` member `name` holding `data`, owned by root, 0644: a 60-byte header of space-padded
/// decimal fields (the mode octal), the data, and a `\n` to an even length.
fn member(out: &mut Vec<u8>, name: &str, data: &[u8], mtime: u64) {
  let head = format!("{name:<16}{mtime:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, "100644", data.len());
  out.extend(head.as_bytes());
  out.extend(data);
  if !out.len().is_multiple_of(2) {
    out.push(b'\n');
  }
}

fn gzip(data: &[u8]) -> io::Result<Vec<u8>> {
  let mut enc = GzEncoder::new(vec![], Compression::best());
  enc.write_all(data)?;
  enc.finish()
}

// MARK: Squashfs

const MAGIC: u32 = 0x7371_7368;
/// A data block's size, and its log2.
const BLOCK: usize = 128 * 1024;
const BLOCK_LOG: u16 = 17;
/// A metadata block's size before compression.
const META: usize = 8192;
/// The compressor id of zlib, which squashfs calls gzip.
const GZIP: u16 = 1;
const NO_FRAGMENTS: u16 = 0x0010;
const NO_XATTRS: u16 = 0x0200;
/// A table's start when the table is absent.
const NONE: u64 = u64::MAX;
/// The superblock's size.
const SUPER: usize = 96;

/// The basic inode types, also a directory entry's type.
const DIR: u16 = 1;
const FILE: u16 = 2;
const LINK: u16 = 3;

/// The image of `tree`, every inode and the image itself of the time `mtime`, padded to 4 KiB.
fn squashfs(tree: &Tree, mtime: u32) -> io::Result<Vec<u8>> {
  let mut out = vec![0; SUPER];

  // The files' data first: a file's inode gives where its blocks start and each one's size.
  let mut blocks = HashMap::new();
  for (path, _, node) in tree.iter() {
    let Node::File(data) = node else { continue };
    let start = u32::try_from(out.len()).map_err(|_| bad("the squashfs is over 4 GiB"))?;
    let len = u32::try_from(data.len()).map_err(|_| bad(format!("{path} is over 4 GiB")))?;
    let mut sizes = vec![];
    for chunk in data.chunks(BLOCK) {
      let (bytes, packed) = compress(chunk);
      // Bit 24 marks a block stored as it is.
      sizes.push(bytes.len() as u32 | if packed { 0 } else { 1 << 24 });
      out.extend(bytes);
    }

    blocks.insert(path, (start, len, sizes));
  }

  // Inodes in post-order, a folder after what it holds: its listing points at their inodes.
  let mut order = vec![];
  post_order(tree, "", &mut order);
  let number: HashMap<&str, u32> =
    order.iter().enumerate().map(|(i, p)| (*p, i as u32 + 1)).collect();
  let count = order.len() as u32;

  let (mut inodes, mut dirs) = (Meta::default(), Meta::default());
  let mut refs = HashMap::new();
  for &path in &order {
    let (mode, node) = match path {
      "" => (0o755, &Node::Dir),
      _ => tree.get(path).ok_or_else(|| bad(format!("no {path} in the tree")))?,
    };

    let kind = match node {
      Node::Dir => DIR,
      Node::File(_) => FILE,
      Node::Link(_) => LINK,
    };

    let mut inode = vec![];
    for field in [kind, mode as u16 & 0o7777, 0, 0] {
      inode.extend(field.to_le_bytes());
    }

    inode.extend(mtime.to_le_bytes());
    inode.extend(number[path].to_le_bytes());
    match node {
      Node::Dir => {
        let (block, offset) = dirs.pos();
        let children: Vec<_> = tree.children(path).collect();
        let listing = listing(tree, &children, &refs, &number)?;
        dirs.write(&listing);
        let size = u16::try_from(listing.len() + 3)
          .map_err(|_| bad(format!("{path}/ holds too many entries")))?;
        let subdirs =
          children.iter().filter(|(_, p)| matches!(tree.get(p), Some((_, Node::Dir)))).count();
        let parent = match path.rsplit_once('/') {
          _ if path.is_empty() => count + 1,
          Some((parent, _)) => number[parent],
          None => number[""],
        };

        inode.extend(block.to_le_bytes());
        inode.extend((2 + subdirs as u32).to_le_bytes());
        inode.extend(size.to_le_bytes());
        inode.extend(offset.to_le_bytes());
        inode.extend(parent.to_le_bytes());
      }
      Node::File(_) => {
        let (start, len, sizes) = &blocks[path];
        for field in [*start, u32::MAX, 0, *len] {
          inode.extend(field.to_le_bytes());
        }

        sizes.iter().for_each(|s| inode.extend(s.to_le_bytes()));
      }
      Node::Link(to) => {
        inode.extend(1u32.to_le_bytes());
        inode.extend((to.len() as u32).to_le_bytes());
        inode.extend(to.as_bytes());
      }
    }

    refs.insert(path, inodes.pos());
    inodes.write(&inode);
  }

  let (block, offset) = refs[""];
  let root = (block as u64) << 16 | offset as u64;
  let inode_table = out.len() as u64;
  out.extend(inodes.finish());
  let dir_table = out.len() as u64;
  out.extend(dirs.finish());

  // The id table: a metadata block of the ids, root's 0 alone, then where that block starts.
  let ids = out.len() as u64;
  let mut id = Meta::default();
  id.write(&0u32.to_le_bytes());
  out.extend(id.finish());
  let id_table = out.len() as u64;
  out.extend(ids.to_le_bytes());
  let used = out.len() as u64;
  out.resize(out.len().next_multiple_of(4096), 0);

  let mut head = vec![];
  for field in [MAGIC, count, mtime, BLOCK as u32, 0] {
    head.extend(field.to_le_bytes());
  }

  // One id, version 4.0.
  for field in [GZIP, BLOCK_LOG, NO_FRAGMENTS | NO_XATTRS, 1, 4, 0] {
    head.extend(field.to_le_bytes());
  }

  // The fragment table, empty, would start where the id table's block does.
  for field in [root, used, id_table, NONE, inode_table, dir_table, ids, NONE] {
    head.extend(field.to_le_bytes());
  }

  out[..SUPER].copy_from_slice(&head);
  Ok(out)
}

/// The paths under `dir`, each folder after what it holds, `dir` itself last.
fn post_order<'a>(tree: &'a Tree, dir: &'a str, out: &mut Vec<&'a str>) {
  for (_, path) in tree.children(dir) {
    match tree.get(path) {
      Some((_, Node::Dir)) => post_order(tree, path, out),
      _ => out.push(path),
    }
  }

  out.push(dir);
}

/// A run of directory entries: their inodes' metadata block, the inode number theirs are relative
/// to, the entries and their count.
struct Run {
  start: u32,
  base: u32,
  entries: Vec<u8>,
  count: u32,
}

/// The directory listing of `children`, `(name, path)` by name, whose inodes are at `refs`: runs
/// of entries, each after a header of their count less one, their inodes' metadata block and the
/// inode number the entries' are relative to. A run holds up to 256 entries of one block.
fn listing(
  tree: &Tree,
  children: &[(&str, &str)],
  refs: &HashMap<&str, (u32, u16)>,
  number: &HashMap<&str, u32>,
) -> io::Result<Vec<u8>> {
  let mut runs: Vec<Run> = vec![];
  for &(name, path) in children {
    // Linux's limit: unsquashfs refuses a longer one.
    if name.is_empty() || name.len() > 255 {
      return Err(bad(format!("{path}: squashfs holds names of 1 to 255 bytes")));
    }

    let ((block, offset), n) = (refs[path], number[path]);
    let fits = |run: &Run| {
      run.start == block && run.count < 256 && i16::try_from(n as i64 - run.base as i64).is_ok()
    };

    if !runs.last().is_some_and(fits) {
      runs.push(Run { start: block, base: n, entries: vec![], count: 0 });
    }

    let kind = match tree.get(path) {
      Some((_, Node::Dir)) => DIR,
      Some((_, Node::Link(_))) => LINK,
      _ => FILE,
    };

    let run = runs.last_mut().unwrap_or_else(|| unreachable!());
    run.entries.extend(offset.to_le_bytes());
    run.entries.extend(((n as i64 - run.base as i64) as i16).to_le_bytes());
    run.entries.extend(kind.to_le_bytes());
    run.entries.extend((name.len() as u16 - 1).to_le_bytes());
    run.entries.extend(name.as_bytes());
    run.count += 1;
  }

  let mut out = vec![];
  for run in runs {
    for field in [run.count - 1, run.start, run.base] {
      out.extend(field.to_le_bytes());
    }

    out.extend(run.entries);
  }

  Ok(out)
}

/// `data` zlib-compressed, and `true`; as it is, and `false`, when that is no smaller.
fn compress(data: &[u8]) -> (Vec<u8>, bool) {
  let mut enc = ZlibEncoder::new(vec![], Compression::best());
  let packed = enc.write_all(data).and_then(|_| enc.finish());
  match packed {
    Ok(packed) if packed.len() < data.len() => (packed, true),
    _ => (data.to_vec(), false),
  }
}

/// A metadata table: its bytes in blocks of [META], each after a 16-bit size whose bit 15 marks
/// one stored as it is. A record may run across blocks.
#[derive(Default)]
struct Meta {
  out: Vec<u8>,
  cur: Vec<u8>,
}

impl Meta {
  /// Where the next byte goes: its block's start in the table and its offset in the block.
  fn pos(&self) -> (u32, u16) {
    (self.out.len() as u32, self.cur.len() as u16)
  }

  fn write(&mut self, mut data: &[u8]) {
    while !data.is_empty() {
      let n = data.len().min(META - self.cur.len());
      self.cur.extend(&data[..n]);
      data = &data[n..];
      if self.cur.len() == META {
        self.flush();
      }
    }
  }

  fn flush(&mut self) {
    if self.cur.is_empty() {
      return;
    }

    let (bytes, packed) = compress(&self.cur);
    let head = bytes.len() as u16 | if packed { 0 } else { 0x8000 };
    self.out.extend(head.to_le_bytes());
    self.out.extend(bytes);
    self.cur.clear();
  }

  fn finish(mut self) -> Vec<u8> {
    self.flush();
    self.out
  }
}

// MARK: Tree

/// What a path holds.
#[derive(Debug, Clone, PartialEq)]
enum Node {
  Dir,
  File(Vec<u8>),
  Link(String),
}

/// The entries under a root folder (0755), each by its path relative to it, `a/b`, with its Unix
/// mode (a link's is 0777). A path's folders are entries too, in byte order before it.
#[derive(Debug, Default)]
struct Tree(BTreeMap<String, (u32, Node)>);

impl Tree {
  /// The file `path` of the mode `mode` holding `data`, its missing folders made 0755.
  pub fn file(&mut self, path: &str, mode: u32, data: impl Into<Vec<u8>>) {
    self.put(path, mode, Node::File(data.into()));
  }

  /// The link `path` to `to`.
  pub fn link(&mut self, path: &str, to: &str) {
    self.put(path, 0o777, Node::Link(to.into()));
  }

  fn put(&mut self, path: &str, mode: u32, node: Node) {
    for (i, _) in path.match_indices('/') {
      self.0.entry(path[..i].to_string()).or_insert((0o755, Node::Dir));
    }

    self.0.insert(path.to_string(), (mode, node));
  }

  /// Each entry as `(path, mode, node)`, a folder before what it holds.
  pub fn iter(&self) -> impl Iterator<Item = (&str, u32, &Node)> {
    self.0.iter().map(|(path, (mode, node))| (path.as_str(), *mode, node))
  }

  /// The entries right in the folder `dir`, `""` the root, as `(name, path)`, by name in byte
  /// order.
  pub fn children<'a>(&'a self, dir: &str) -> impl Iterator<Item = (&'a str, &'a str)> {
    let prefix = match dir.is_empty() {
      true => String::new(),
      false => format!("{dir}/"),
    };

    // Every path under `dir/` sorts in one range from it, folders' contents among them.
    let under = self.0.range(prefix.clone()..).map(|(path, _)| path.as_str());
    let under = under.take_while(move |path| path.starts_with(&prefix));
    let skip = dir.len() + usize::from(!dir.is_empty());
    under.filter_map(move |path| {
      let name = &path[skip..];
      (!name.contains('/')).then_some((name, path))
    })
  }

  /// The entry at `path`.
  pub fn get(&self, path: &str) -> Option<(u32, &Node)> {
    self.0.get(path).map(|(mode, node)| (*mode, node))
  }

  /// The bytes of all files.
  pub fn size(&self) -> u64 {
    let len = |(_, node): &(u32, Node)| match node {
      Node::File(data) => data.len() as u64,
      _ => 0,
    };

    self.0.values().map(len).sum()
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use std::io::Read;
  #[cfg(target_os = "linux")]
  use std::process::Command;
  use std::time::Duration;

  use flate2::read::{GzDecoder, ZlibDecoder};

  use super::*;
  #[cfg(target_os = "linux")]
  use crate::tool::run;
  use crate::tool::{Temp, get};

  /// A config in `dir` with the `[linux]` table `linux`.
  fn config(dir: &Path, linux: &str) -> Config {
    Config {
      id: "com.example.hello".into(),
      name: "Hello World".into(),
      icon: dir.join("icon.png"),
      comment: "Says hello".into(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos: Default::default(),
      windows: Default::default(),
      linux: toml::from_str(linux).unwrap(),
      publisher: "Example".into(),
      authors: vec!["Jane Doe <jane@example.com>".into()],
      repository: String::new(),
      root: dir.to_path_buf(),
      workspace: dir.to_path_buf(),
      target: dir.join("target"),
    }
  }

  const LINUX: &str = r#"
categories = ["AudioVideo", "Audio"]
deb-depends = ["libc6 (>= 2.31)"]
deb-recommends = ["libgtk-3-0t64 | libgtk-3-0", "libasound2t64"]
"#;

  #[test]
  fn desktop_entry() {
    let mut cfg = config(Path::new("/app"), LINUX);
    let want = "[Desktop Entry]\nType=Application\nName=Hello World\nComment=Says hello\n\
      Exec=hello\nIcon=com.example.hello\nStartupWMClass=com.example.hello\nStartupNotify=true\n\
      Terminal=false\nCategories=AudioVideo;Audio;\n";
    assert_eq!(desktop(&cfg).unwrap(), want);

    cfg.comment = String::new();
    cfg.linux = Default::default();
    let plain = desktop(&cfg).unwrap();
    assert!(!plain.contains("Comment="), "{plain}");
    assert!(plain.ends_with("Categories=Utility;\n"), "{plain}");

    cfg.name = " Hi\\there\t ".into();
    cfg.comment = "line\nnext\r".into();
    cfg.bin = "my app".into();
    cfg.linux = toml::from_str("categories = [\"X-Odd;One\"]").unwrap();
    let odd = desktop(&cfg).unwrap();
    assert!(odd.contains("\nName=\\sHi\\\\there\\t\\s\n"), "{odd}");
    assert!(odd.contains("\nComment=line\\nnext\\r\n"), "{odd}");
    assert!(odd.contains("\nExec=\"my app\"\n"), "{odd}");
    assert!(odd.ends_with("\nCategories=X-Odd\\;One;\n"), "{odd}");
  }

  #[test]
  fn exec_args() {
    let cases = [
      ("hello", "hello"),
      ("hello-app_2", "hello-app_2"),
      ("100%", "100%%"),
      ("my app", "\"my app\""),
      ("a$b`c\"d", "\"a\\$b\\`c\\\"d\""),
      ("it's", "\"it's\""),
    ];

    for (arg, want) in cases {
      assert_eq!(exec(arg), want, "{arg}");
    }

    // Quoted for Exec, then escaped as a string: a `\` is four.
    assert_eq!(escape(&exec("a\\b")), "\"a\\\\\\\\b\"");
  }

  #[test]
  fn control_file() {
    let cfg = config(Path::new("/app"), LINUX);
    let want = "Package: hello\nVersion: 1.2.0~beta.1~dev\nArchitecture: arm64\n\
      Maintainer: Example <jane@example.com>\nInstalled-Size: 3\nDepends: libc6 (>= 2.31)\n\
      Recommends: libgtk-3-0t64 | libgtk-3-0, libasound2t64\nSection: utils\n\
      Priority: optional\nDescription: Says hello\n";
    assert_eq!(control(&cfg, "hello", Arch::Aarch64, true, 3000).unwrap(), want);

    let mut cfg = config(Path::new("/app"), "");
    cfg.comment = String::new();
    cfg.authors = vec!["Joe".into(), "Jane <jane@example.com>".into()];
    let want = "Package: hello\nVersion: 1.2.0~beta.1\nArchitecture: amd64\n\
      Maintainer: Example <jane@example.com>\nInstalled-Size: 1\nSection: utils\n\
      Priority: optional\nDescription: Hello World\n";
    assert_eq!(control(&cfg, "hello", Arch::X86_64, false, 1).unwrap(), want);

    cfg.linux = toml::from_str("deb-depends = [\"a\\nb\"]").unwrap();
    let e = control(&cfg, "hello", Arch::X86_64, false, 1).unwrap_err().to_string();
    assert_eq!(e, "the .deb's Depends is one line, not \"a\\nb\"");
    cfg.linux = Default::default();

    for authors in [vec![], vec!["Jane".to_string()], vec!["Jane <jane>".to_string()]] {
      cfg.authors = authors;
      let e = control(&cfg, "hello", Arch::X86_64, false, 1).unwrap_err().to_string();
      assert!(e.starts_with("the .deb's Maintainer is `Name <mail>`"), "{e}");
    }
  }

  #[test]
  fn descriptions() {
    let mut cfg = config(Path::new("/app"), "");
    let cases = [
      ("Says hello", "Description: Says hello\n"),
      ("  Says hello \n\n \n", "Description: Says hello\n"),
      ("", "Description: Hello World\n"),
      (
        "Says hello\n\nTo you.\r\n\n  In two lines.\n\n",
        "Description: Says hello\n To you.\n .\n   In two lines.\n",
      ),
      ("Says hello\nTo you.", "Description: Says hello\n To you.\n"),
    ];

    for (comment, want) in cases {
      cfg.comment = comment.into();
      assert_eq!(description(&cfg), want, "{comment:?}");
    }
  }

  #[test]
  fn package_names() {
    let mut cfg = config(Path::new("/app"), "");
    assert_eq!(package(&cfg).unwrap(), "hello");
    cfg.bin = "Hello_App".into();
    assert_eq!(package(&cfg).unwrap(), "hello-app");
    cfg.linux = toml::from_str("deb-package = \"hello2.0+x\"").unwrap();
    assert_eq!(package(&cfg).unwrap(), "hello2.0+x");

    for name in ["Hello", "h", "-hello", "hello world", "héllo"] {
      cfg.linux = toml::from_str(&format!("deb-package = \"{name}\"")).unwrap();
      let e = package(&cfg).unwrap_err().to_string();
      assert!(e.contains("is no Debian package name"), "{name}: {e}");
    }
  }

  /// Cargo's versions, a dev build's or not, each before the next as dpkg orders them.
  const ORDERED: [(&str, bool, &str); 13] = [
    ("1.1.9", false, "1.1.9"),
    ("1.2.0-beta.2", true, "1.2.0~beta.2~dev"),
    ("1.2.0-beta.2", false, "1.2.0~beta.2"),
    ("1.2.0-beta.10", false, "1.2.0~beta.10"),
    ("1.2.0-rc.1", false, "1.2.0~rc.1"),
    ("1.2.0-rc.2+build-5", false, "1.2.0~rc.2"),
    ("1.2.0-rc-3", false, "1.2.0~rc.3"),
    ("1.2.0-rc.4-x+y-z", false, "1.2.0~rc.4.x"),
    ("1.2.0", false, "1.2.0"),
    ("1.2.1+build-5", true, "1.2.1~dev"),
    ("1.2.1+build-5", false, "1.2.1"),
    ("1.10.0-alpha", false, "1.10.0~alpha"),
    ("1.10.0", false, "1.10.0"),
  ];

  #[test]
  fn deb_versions() {
    for (version, dev, want) in ORDERED {
      assert_eq!(deb_version(version, dev), want, "{version} {dev}");
    }

    assert_eq!(deb_version("1.2.0-rc.1+build-5", true), "1.2.0~rc.1~dev");
  }

  /// [ORDERED] as dpkg orders them, each dev build before the version it builds.
  #[cfg(target_os = "linux")]
  #[test]
  fn dpkg_orders() {
    let mut pairs: Vec<_> = ORDERED.windows(2).map(|w| (w[0].2, w[1].2)).collect();
    pairs.extend([("1.2.0~dev", "1.2.0"), ("1.2.0~rc.1~dev", "1.2.0~rc.1")]);
    // A `-` left in the pre-release would be a revision: `1.2.0~rc-3` is `1.2.0~rc`, revision 3.
    pairs.push(("1.2.0~rc-3", "1.2.0~rc.3"));
    for (a, b) in pairs {
      run(Command::new("dpkg").args(["--compare-versions", a, "lt", b])).unwrap();
    }
  }

  #[test]
  fn appdir_layout() {
    let cfg = config(Path::new("/app"), LINUX);
    let tree = appdir(&cfg, b"exe".to_vec(), b"png".to_vec()).unwrap();
    let files: Vec<_> = tree
      .iter()
      .map(|(path, mode, node)| match node {
        Node::Link(to) => format!("{path} -> {to}"),
        _ => format!("{path} {mode:o}"),
      })
      .collect();
    let want = [
      ".DirIcon -> com.example.hello.png",
      "AppRun -> usr/bin/hello",
      "com.example.hello.desktop 644",
      "com.example.hello.png 644",
      "usr 755",
      "usr/bin 755",
      "usr/bin/hello 755",
    ];
    assert_eq!(files, want);
    assert_eq!(tree.get("usr/bin/hello"), Some((0o755, &Node::File(b"exe".to_vec()))));
    assert_eq!(tree.get("com.example.hello.png"), Some((0o644, &Node::File(b"png".to_vec()))));
    let entry = Node::File(desktop(&cfg).unwrap().into_bytes());
    assert_eq!(tree.get("com.example.hello.desktop"), Some((0o644, &entry)));
  }

  /// The binary's time, unless `$SOURCE_DATE_EPOCH` gives one.
  #[test]
  fn times() {
    if std::env::var_os("SOURCE_DATE_EPOCH").is_some() {
      return;
    }

    let tmp = Temp::new().unwrap();
    let bin = tmp.join("bin");
    let file = File::create(&bin).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
    assert_eq!(mtime(&bin).unwrap(), 1_700_000_000);
    let e = mtime(&tmp.join("none")).unwrap_err().to_string();
    assert!(e.starts_with(&format!("{}: ", tmp.join("none").display())), "{e}");
  }

  #[test]
  fn icon_sizes() {
    let tmp = Temp::new().unwrap();
    let cfg = config(&tmp, "");
    // A 1024 px icon with a margin of 112 px, as macOS keeps: cropped to its tile, then scaled.
    let px = (0..1024 * 1024)
      .flat_map(|i| {
        let (x, y) = (i % 1024, i / 1024);
        let inside = (112..912).contains(&x) && (112..912).contains(&y);
        [200, 100, 50, if inside { 255 } else { 0 }]
      })
      .collect();
    fs::write(&cfg.icon, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
    let pngs = pngs(&cfg, &HICOLOR).unwrap();
    assert_eq!(pngs.keys().copied().collect::<Vec<_>>(), HICOLOR);
    for (n, png) in pngs {
      let png = Image::decode(&png).unwrap();
      assert_eq!((png.w, png.h), (n, n));
      assert!(png.px.as_chunks::<4>().0.iter().all(|p| *p == [200, 100, 50, 255]), "{n}");
    }
  }

  #[test]
  fn licenses() {
    let tmp = Temp::new().unwrap();
    let mut cfg = config(&tmp.join("ws/app"), "");
    cfg.workspace = tmp.join("ws");
    fs::create_dir_all(&cfg.root).unwrap();
    fs::write(tmp.join("LICENSE"), "outside").unwrap();
    assert_eq!(license(&cfg), None);

    fs::write(cfg.workspace.join("LICENSE"), "workspace").unwrap();
    assert_eq!(license(&cfg), Some(cfg.workspace.join("LICENSE")));
    fs::write(cfg.root.join("LICENSE"), "own").unwrap();
    assert_eq!(license(&cfg), Some(cfg.root.join("LICENSE")));
  }

  /// The lines of `tar -tv`-like `text` (`dpkg-deb --contents`, `unsquashfs -ll`) as
  /// `<mode> <path>[ -> <link>]`, sorted; each owned by root, else it panics.
  #[cfg(target_os = "linux")]
  fn listing(text: &str) -> Vec<String> {
    let lines = text.lines().filter(|l| !l.is_empty() && !l.starts_with("Parallel unsquashfs"));
    let lines = lines.map(|line| {
      let parts: Vec<_> = line.split_whitespace().collect();
      assert_eq!(parts.get(1), Some(&"root/root"), "{line}");
      format!("{} {}", parts[0], parts[5..].join(" "))
    });
    sorted(lines.collect())
  }

  #[cfg(target_os = "linux")]
  fn sorted(mut lines: Vec<String>) -> Vec<String> {
    lines.sort();
    lines
  }

  /// Both packages of a binary, read back by the system's tools: `unsquashfs` and the runtime,
  /// `dpkg-deb` and `desktop-file-validate`. The binary is `$SHIPO_TEST_ELF`, else a script, and
  /// prints `hello 1.2.0-beta.1`; the packages stay in `$SHIPO_TEST_OUT` when it is set.
  #[cfg(target_os = "linux")]
  #[test]
  #[ignore = "fetches the AppImage runtime from GitHub; needs squashfs-tools, desktop-file-utils"]
  fn packages() {
    use std::env;

    let tmp = Temp::new().unwrap();
    let mut cfg = config(&tmp, LINUX);
    // The target folder of this test's `<target>/debug/deps/<test>`, where the runtime stays.
    cfg.target = env::current_exe().unwrap().ancestors().nth(3).unwrap().to_path_buf();
    let px = (0..1024 * 1024).flat_map(|i| [(i % 256) as u8, (i / 4096) as u8, 128, 255]).collect();
    fs::write(&cfg.icon, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
    fs::write(tmp.join("LICENSE"), "MIT").unwrap();
    let bin = env::var_os("SHIPO_TEST_ELF").map_or_else(
      || {
        let path = tmp.join("hello");
        fs::write(&path, "#!/bin/sh\necho hello 1.2.0-beta.1\n").unwrap();
        path
      },
      PathBuf::from,
    );

    let out = env::var_os("SHIPO_TEST_OUT").map_or_else(|| tmp.join("out"), PathBuf::from);
    fs::create_dir_all(&out).unwrap();
    let arch = match env::consts::ARCH {
      "aarch64" => Arch::Aarch64,
      _ => Arch::X86_64,
    };

    let text = |cmd: &mut Command| String::from_utf8(run(cmd).unwrap()).unwrap();

    // Both made first, so a host that can't run the AppImage still checks the .deb: an emulator's
    // binfmt entry refuses the AppImage's magic bytes in the ELF header.
    let image = appimage(&cfg, &bin, arch, &out).unwrap();
    let deb = deb(&cfg, &bin, arch, true, &out).unwrap();
    assert_eq!(deb, out.join(format!("Hello World-dev-linux-{arch}.deb")));
    let info = text(Command::new("dpkg-deb").arg("--info").arg(&deb));
    let control = control(&cfg, "hello", arch, true, 0).unwrap();
    for line in control.lines().filter(|l| !l.starts_with("Installed-Size")) {
      assert!(info.contains(&format!("\n {line}\n")), "{line}: {info}");
    }

    let mut want: Vec<String> = [
      "drwxr-xr-x ./",
      "drwxr-xr-x ./usr/",
      "drwxr-xr-x ./usr/bin/",
      "-rwxr-xr-x ./usr/bin/hello",
      "drwxr-xr-x ./usr/share/",
      "drwxr-xr-x ./usr/share/applications/",
      "-rw-r--r-- ./usr/share/applications/com.example.hello.desktop",
      "drwxr-xr-x ./usr/share/doc/",
      "drwxr-xr-x ./usr/share/doc/hello/",
      "-rw-r--r-- ./usr/share/doc/hello/copyright",
      "drwxr-xr-x ./usr/share/icons/",
      "drwxr-xr-x ./usr/share/icons/hicolor/",
    ]
    .map(String::from)
    .to_vec();
    for n in HICOLOR {
      let dir = format!("./usr/share/icons/hicolor/{n}x{n}");
      want.push(format!("drwxr-xr-x {dir}/"));
      want.push(format!("drwxr-xr-x {dir}/apps/"));
      want.push(format!("-rw-r--r-- {dir}/apps/com.example.hello.png"));
    }

    let contents = text(Command::new("dpkg-deb").arg("--contents").arg(&deb));
    assert_eq!(listing(&contents), sorted(want));

    let files = tmp.join("deb");
    run(Command::new("dpkg-deb").arg("-x").arg(&deb).arg(&files)).unwrap();
    let entry = files.join("usr/share/applications/com.example.hello.desktop");
    assert_eq!(text(Command::new("desktop-file-validate").arg(&entry)), "");
    for n in HICOLOR {
      let png = files.join(format!("usr/share/icons/hicolor/{n}x{n}/apps/com.example.hello.png"));
      assert_eq!(Image::read(&png).map(|p| (p.w, p.h)).unwrap(), (n, n));
    }

    assert_eq!(image, out.join(format!("Hello World-dev-linux-{arch}.AppImage")));
    let offset = text(Command::new(&image).arg("--appimage-offset"));
    let runtime = cfg.target.join(format!("tools/runtime-{RUNTIME}-{arch}"));
    assert_eq!(offset.trim(), fs::metadata(&runtime).unwrap().len().to_string());
    let list = text(Command::new("unsquashfs").args(["-ll", "-o", offset.trim()]).arg(&image));
    let want = [
      "drwxr-xr-x squashfs-root",
      "lrwxrwxrwx squashfs-root/.DirIcon -> com.example.hello.png",
      "lrwxrwxrwx squashfs-root/AppRun -> usr/bin/hello",
      "-rw-r--r-- squashfs-root/com.example.hello.desktop",
      "-rw-r--r-- squashfs-root/com.example.hello.png",
      "drwxr-xr-x squashfs-root/usr",
      "drwxr-xr-x squashfs-root/usr/bin",
      "-rwxr-xr-x squashfs-root/usr/bin/hello",
    ];
    assert_eq!(listing(&list), sorted(want.map(String::from).to_vec()));

    let said = text(Command::new(&image).arg("--appimage-extract-and-run"));
    assert_eq!(said, "hello 1.2.0-beta.1\n");
  }

  /// The members of the `ar` archive `data` as `(header, data)`, checking the layout on the way.
  fn members(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(&data[..8], b"!<arch>\n");
    let (mut out, mut at) = (vec![], 8);
    while at < data.len() {
      let head = std::str::from_utf8(&data[at..at + 60]).unwrap().to_string();
      assert_eq!(&head[58..], "`\n");
      let size: usize = head[48..58].trim_end().parse().unwrap();
      out.push((head, data[at + 60..at + 60 + size].to_vec()));
      at += 60 + size;
      if size % 2 == 1 {
        assert_eq!(data[at], b'\n');
        at += 1;
      }
    }

    assert_eq!(at, data.len());
    out
  }

  fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    GzDecoder::new(data).read_to_end(&mut out).unwrap();
    out
  }

  #[test]
  fn layout() {
    let mut tree = Tree::default();
    tree.file("usr/bin/hello", 0o755, "exe");
    tree.file("usr/share/doc/hello/copyright", 0o644, "MIT");
    let deb = deb_archive("Package: hello\n", &tree, 1_700_000_000).unwrap();

    let members = members(&deb);
    let heads: Vec<_> = members.iter().map(|(head, _)| head[..48].to_string()).collect();
    let want = ["debian-binary", "control.tar.gz", "data.tar.gz"]
      .map(|name| format!("{name:<16}1700000000  0     0     100644  "));
    assert_eq!(heads, want);
    assert_eq!(members[0].1, b"2.0\n");

    let mut meta = Tree::default();
    meta.file("control", 0o644, "Package: hello\n");
    assert_eq!(gunzip(&members[1].1), archive(&meta, 1_700_000_000).unwrap());
    assert_eq!(gunzip(&members[2].1), archive(&tree, 1_700_000_000).unwrap());
  }

  #[test]
  fn archived() {
    let mut tree = Tree::default();
    let deep = format!("{}/{}/{}", "a".repeat(90), "c".repeat(60), "b".repeat(90));
    let long = format!("{}/b", "a".repeat(260));
    tree.file("usr/bin/hello", 0o755, "exe");
    tree.link("usr/bin/hi", "hello");
    tree.link("usr/bin/far", &"x".repeat(101));
    tree.file(&deep, 0o644, vec![7; 600]);
    tree.file(&long, 0o600, "long");
    let data = archive(&tree, 1_700_000_000).unwrap();
    assert_eq!(data, archive(&tree, 1_700_000_000).unwrap());

    // Each entry by its full name, type, mode, link and data; ustar, root's, of the one time.
    let mut entries = vec![];
    for entry in tar::Archive::new(&data[..]).entries().unwrap() {
      let mut entry = entry.unwrap();
      let head = entry.header().clone();
      assert!(head.as_ustar().is_some());
      assert_eq!((head.uid().unwrap(), head.gid().unwrap()), (0, 0));
      assert_eq!(
        (head.username().unwrap(), head.groupname().unwrap()),
        (Some("root"), Some("root"))
      );
      assert_eq!(head.mtime().unwrap(), 1_700_000_000);

      let name = String::from_utf8(entry.path_bytes().into_owned()).unwrap();
      let link = entry.link_name_bytes().map(|x| String::from_utf8(x.into_owned()).unwrap());
      let mut body = vec![];
      entry.read_to_end(&mut body).unwrap();
      let kind = head.entry_type().as_byte() as char;
      let mode = head.mode().unwrap();
      entries.push(format!("{name} {kind} {mode:o} {} {}", link.unwrap_or_default(), body.len()));
    }

    let want = [
      "./ 5 755  0".to_string(),
      format!("./{}/ 5 755  0", "a".repeat(90)),
      format!("./{}/{}/ 5 755  0", "a".repeat(90), "c".repeat(60)),
      format!("./{deep} 0 644  600"),
      format!("./{}/ 5 755  0", "a".repeat(260)),
      format!("./{long} 0 600  4"),
      "./usr/ 5 755  0".into(),
      "./usr/bin/ 5 755  0".into(),
      format!("./usr/bin/far 2 777 {} 0", "x".repeat(101)),
      "./usr/bin/hello 0 755  3".into(),
      "./usr/bin/hi 2 777 hello 0".into(),
    ];
    assert_eq!(entries, want);
  }

  fn u16le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(get(data, at).unwrap())
  }

  fn u32le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(get(data, at).unwrap())
  }

  fn u64le(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(get(data, at).unwrap())
  }

  fn inflate(data: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    ZlibDecoder::new(data).read_to_end(&mut out).unwrap();
    out
  }

  /// The metadata table from `start` to `end` of `img`, unpacked, and each block's start in it by
  /// its start in the table.
  fn table(img: &[u8], start: u64, end: u64) -> (Vec<u8>, HashMap<u32, usize>) {
    let (mut out, mut at, mut starts) = (vec![], start as usize, HashMap::new());
    while at < end as usize {
      let head = u16le(img, at);
      let size = (head & 0x7FFF) as usize;
      let block = &img[at + 2..at + 2 + size];
      starts.insert((at - start as usize) as u32, out.len());
      let data = if head & 0x8000 != 0 { block.to_vec() } else { inflate(block) };
      assert!(data.len() <= META);
      out.extend(data);
      at += 2 + size;
    }

    assert_eq!(at, end as usize);
    (out, starts)
  }

  /// What a read of the image `img` finds, as a reader walks it from the root inode.
  struct Squash<'a> {
    img: &'a [u8],
    inodes: (Vec<u8>, HashMap<u32, usize>),
    dirs: (Vec<u8>, HashMap<u32, usize>),
  }

  impl Squash<'_> {
    /// The entries under the folder of the inode at `at` in the inode table, by path under
    /// `prefix`, as `(mode, node)`, checking each inode's fields on the way: the folder's parent
    /// is `parent`, and its names are in strictly increasing byte order.
    fn walk(&self, at: usize, prefix: &str, parent: u32, out: &mut Vec<(String, u32, Node)>) {
      let ino = &self.inodes.0;
      assert_eq!(u16le(ino, at), DIR);
      assert_eq!(u32le(ino, at + 28), parent, "{prefix}");
      let me = u32le(ino, at + 12);
      let mut last: Option<String> = None;
      let (block, nlink) = (u32le(ino, at + 16), u32le(ino, at + 20));
      let (size, offset) = (u16le(ino, at + 24) as usize, u16le(ino, at + 26) as usize);
      let mut pos = self.dirs.1[&block] + offset;
      let end = pos + size - 3;
      let mut subdirs = 0;
      while pos < end {
        let d = &self.dirs.0;
        let (count, start, base) = (u32le(d, pos) + 1, u32le(d, pos + 4), u32le(d, pos + 8));
        pos += 12;
        for _ in 0..count {
          let offset = u16le(d, pos) as usize;
          let n = (base as i64 + i16::from_le_bytes([d[pos + 2], d[pos + 3]]) as i64) as u32;
          let kind = u16le(d, pos + 4);
          let len = u16le(d, pos + 6) as usize + 1;
          let name = std::str::from_utf8(&d[pos + 8..pos + 8 + len]).unwrap();
          let path = format!("{prefix}{name}");
          pos += 8 + len;
          assert!(last.as_deref().is_none_or(|l| l.as_bytes() < name.as_bytes()), "{path}");
          last = Some(name.to_string());

          let at = self.inodes.1[&start] + offset;
          assert_eq!((u16le(ino, at), u32le(ino, at + 12)), (kind, n), "{path}");
          assert_eq!((u16le(ino, at + 4), u16le(ino, at + 6)), (0, 0), "{path}");
          let mode = u16le(ino, at + 2) as u32;
          match kind {
            DIR => {
              subdirs += 1;
              out.push((path.clone(), mode, Node::Dir));
              self.walk(at, &format!("{path}/"), me, out);
            }
            FILE => out.push((path, mode, Node::File(self.data(at)))),
            _ => {
              let len = u32le(ino, at + 20) as usize;
              let to = String::from_utf8(ino[at + 24..at + 24 + len].to_vec()).unwrap();
              out.push((path, mode, Node::Link(to)));
            }
          }
        }
      }

      assert_eq!(pos, end);
      assert_eq!(nlink, 2 + subdirs);
    }

    /// The data of the file inode at `at`.
    fn data(&self, at: usize) -> Vec<u8> {
      let ino = &self.inodes.0;
      let (mut pos, frag, len) =
        (u32le(ino, at + 16) as usize, u32le(ino, at + 20), u32le(ino, at + 28));
      assert_eq!(frag, u32::MAX);
      let mut out = vec![];
      for i in 0..(len as usize).div_ceil(BLOCK) {
        let size = u32le(ino, at + 32 + 4 * i);
        let n = (size & 0xFF_FFFF) as usize;
        let block = &self.img[pos..pos + n];
        let block = if size & 1 << 24 != 0 { block.to_vec() } else { inflate(block) };
        assert!(block.len() == BLOCK || out.len() + block.len() == len as usize);
        out.extend(block);
        pos += n;
      }

      assert_eq!(out.len(), len as usize);
      out
    }
  }

  #[test]
  fn image() {
    let mut tree = Tree::default();
    tree.file("usr/bin/hello", 0o755, "#!/bin/sh\necho hello\n");
    tree.file("hello.desktop", 0o644, "[Desktop Entry]\n");
    tree.file("empty", 0o600, "");
    tree.link("AppRun", "usr/bin/hello");
    // Three blocks: one packs, one doesn't, and a short last one.
    let mut x = 1u32;
    let noise = (0..BLOCK).map(|_| {
      x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
      (x >> 16) as u8
    });
    let big: Vec<u8> = vec![0; BLOCK].into_iter().chain(noise).chain([1; 100]).collect();
    tree.file("usr/lib/big", 0o644, big.clone());
    // Enough entries for runs across metadata blocks and past 256 to a header.
    for i in 0..600 {
      tree.file(&format!("many/f{i:03}-{}", "x".repeat(40)), 0o644, format!("{i}"));
    }

    let img = squashfs(&tree, 1_700_000_000).unwrap();
    assert_eq!(img.len() % 4096, 0);
    assert_eq!(u32le(&img, 0), MAGIC);
    let count = tree.iter().count() as u32 + 1;
    assert_eq!(u32le(&img, 4), count);
    assert_eq!(
      (u32le(&img, 8), u32le(&img, 12), u32le(&img, 16)),
      (1_700_000_000, BLOCK as u32, 0)
    );
    let shorts: Vec<_> = (20..32).step_by(2).map(|at| u16le(&img, at)).collect();
    assert_eq!(shorts, [GZIP, BLOCK_LOG, NO_FRAGMENTS | NO_XATTRS, 1, 4, 0]);
    let longs: Vec<_> = (32..96).step_by(8).map(|at| u64le(&img, at)).collect();
    let [root, used, id_table, xattrs, inode_table, dir_table, frags, export] = longs[..] else {
      unreachable!()
    };
    assert_eq!((xattrs, export), (NONE, NONE));
    // No fragments: their empty table starts where the next one, the id table's block, does.
    assert!(inode_table < dir_table && dir_table < frags);
    assert_eq!(frags, u64le(&img, id_table as usize));
    assert_eq!(used, id_table + 8);
    assert!(used as usize <= img.len());

    let ids = table(&img, u64le(&img, id_table as usize), id_table);
    assert_eq!(ids.0, [0; 4]);

    let image = Squash {
      img: &img,
      inodes: table(&img, inode_table, dir_table),
      dirs: table(&img, dir_table, u64le(&img, id_table as usize)),
    };
    assert!(image.inodes.1.len() > 2 && image.dirs.1.len() > 2);
    let at = image.inodes.1[&((root >> 16) as u32)] + (root & 0xFFFF) as usize;
    let mut found = vec![];
    image.walk(at, "", count + 1, &mut found);

    let want: Vec<_> = tree.iter().map(|(p, m, n)| (p.to_string(), m, n.clone())).collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(found, want);

    // The noise is stored as it is, the zeros packed.
    assert!(compress(&big[..BLOCK]).1 && !compress(&big[BLOCK..2 * BLOCK]).1);
  }

  #[test]
  fn names() {
    let mut tree = Tree::default();
    tree.file(&"x".repeat(255), 0o644, "");
    assert!(squashfs(&tree, 0).is_ok());
    tree.file(&"x".repeat(256), 0o644, "");
    let e = squashfs(&tree, 0).unwrap_err().to_string();
    assert!(e.ends_with("squashfs holds names of 1 to 255 bytes"), "{e}");
  }

  #[test]
  fn entries() {
    let mut tree = Tree::default();
    tree.file("usr/bin/hello", 0o755, "exe");
    tree.file("usr-x", 0o644, "x");
    tree.link("AppRun", "usr/bin/hello");
    tree.file("usr/share/doc/c", 0o644, "c");

    let all: Vec<_> = tree.iter().map(|(path, mode, _)| format!("{path} {mode:o}")).collect();
    let want = [
      "AppRun 777",
      "usr 755",
      "usr-x 644",
      "usr/bin 755",
      "usr/bin/hello 755",
      "usr/share 755",
      "usr/share/doc 755",
      "usr/share/doc/c 644",
    ];
    assert_eq!(all, want);

    let names = |dir| tree.children(dir).map(|(name, _)| name).collect::<Vec<_>>();
    assert_eq!(names(""), ["AppRun", "usr", "usr-x"]);
    assert_eq!(names("usr"), ["bin", "share"]);
    assert_eq!(names("usr/share/doc"), ["c"]);
    assert_eq!(tree.get("usr/bin"), Some((0o755, &Node::Dir)));
    assert_eq!(tree.size(), 5);
  }
}
