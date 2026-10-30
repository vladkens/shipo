// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! An HFS+ volume (Apple's TN1150) of a tree of folders, files and symlinks, as a disk's bytes:
//! case-insensitive, not journaled, written once and never grown, so it has no free space.
//!
//! The volume, in 4 KiB blocks: block 0 holds the volume header at 1024, then the allocation
//! bitmap, the extents tree (its header node alone: each fork is one extent), the catalog tree,
//! the files' data forks one after another, and the last block, whose end holds the alternate
//! volume header.

use std::cmp::Ordering;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::unicode::{CCC, NFD};
use crate::{at, bad, tool};

/// The allocation block's size.
const BLOCK: usize = 4096;

/// Seconds from 1904-01-01, HFS+'s epoch, to 1970-01-01.
const EPOCH: u64 = 2_082_844_800;

/// The catalog node ids of the root's parent and the root; ids below 16 are Apple's.
const ROOT_PARENT: u32 = 1;
const ROOT: u32 = 2;
const FIRST_ID: u32 = 16;

/// Catalog record types.
const FOLDER: u16 = 1;
const FILE: u16 = 2;
const FOLDER_THREAD: u16 = 3;
const FILE_THREAD: u16 = 4;

/// The file type bits of a mode.
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

/// An entry of the tree the volume holds.
#[derive(Debug, Clone)]
pub struct Entry {
  pub name: String,
  /// The permission bits, the file type not among them.
  pub mode: u16,
  /// Its modification time, in seconds since 1970.
  pub time: u64,
  pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
  Dir(Vec<Entry>),
  File(Data),
  /// A symlink to its target.
  Link(String),
}

/// A file's data: its bytes, or the file on disk of that size, read straight into the volume.
#[derive(Debug, Clone)]
pub enum Data {
  Bytes(Vec<u8>),
  Disk(PathBuf, u64),
}

impl Entry {
  /// The tree at `path` as it is on disk: names, and symlinks as links, each folder's entries by
  /// name, whatever order the host lists them in; the times [tool::time]'s, the modes the writer's
  /// ([mode]), the owners root's, so any host writes the same bytes of the same tree.
  pub fn read(path: &Path) -> io::Result<Entry> {
    let at = at(path);
    let meta = fs::symlink_metadata(path).map_err(&at)?;
    let name = path.file_name().unwrap_or_default();
    let Some(name) = name.to_str() else {
      return Err(bad(format!("{}: a name not in UTF-8", path.display())));
    };

    let kind = match meta.file_type() {
      t if t.is_symlink() => {
        let target = fs::read_link(path).map_err(&at)?;
        let Some(target) = target.to_str() else {
          return Err(bad(format!("{}: a link not in UTF-8", path.display())));
        };

        Kind::Link(target.into())
      }
      t if t.is_dir() => {
        let mut entries = vec![];
        for entry in fs::read_dir(path).map_err(&at)? {
          entries.push(Entry::read(&entry.map_err(&at)?.path())?);
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Kind::Dir(entries)
      }
      t if t.is_file() => Kind::File(Data::Disk(path.to_path_buf(), meta.len())),
      // A FIFO would block the read, a device or a socket has no data to keep.
      _ => return Err(bad(format!("{}: not a file, folder or symlink", path.display()))),
    };

    let time = tool::time(&meta).map_err(&at)?;
    Ok(Entry { name: name.into(), mode: mode(&meta, &kind), time, kind })
  }
}

/// The permission bits of `meta`, set as the writer's own, never the build host's `umask` (a
/// 0700 bundle opens for its owner alone): folders and files with an execute bit 0755, other
/// files 0644, links as they are. Off Unix, which keeps no mode on disk, a file executes when
/// [program] says so.
fn mode(meta: &fs::Metadata, kind: &Kind) -> u16 {
  #[cfg(unix)]
  let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) as u16 & 0o7777;
  #[cfg(not(unix))]
  let (mode, _) = (off_unix(kind), meta);

  match kind {
    Kind::Link(_) => mode,
    Kind::File(_) if mode & 0o111 == 0 => 0o644,
    Kind::File(_) | Kind::Dir(_) => 0o755,
  }
}

/// The mode off Unix of an entry: 0755 for a folder, a link and a [program] on disk, else 0644.
#[cfg_attr(unix, allow(dead_code))]
fn off_unix(kind: &Kind) -> u16 {
  let head = match kind {
    Kind::File(Data::Disk(path, _)) => {
      let mut head = [0; 4];
      let read = fs::File::open(path).and_then(|mut f| io::Read::read(&mut f, &mut head));
      head[..read.unwrap_or(0)].to_vec()
    }
    _ => vec![],
  };

  match kind {
    Kind::File(_) if !program(&head) => 0o644,
    _ => 0o755,
  }
}

/// Whether a file of the first bytes `head` is a program: a Mach-O, thin or fat (32- or 64-bit
/// offsets), or a script.
#[cfg_attr(unix, allow(dead_code))]
fn program(head: &[u8]) -> bool {
  const MACHO: [[u8; 4]; 4] = [
    [0xCF, 0xFA, 0xED, 0xFE],
    [0xCE, 0xFA, 0xED, 0xFE],
    [0xCA, 0xFE, 0xBA, 0xBE],
    [0xCA, 0xFE, 0xBA, 0xBF],
  ];
  MACHO.iter().any(|m| head.starts_with(m)) || head.starts_with(b"#!")
}

// MARK: Names

/// `name` as HFS+ stores it: decomposed (NFD) by Unicode 3.2 as TN1150 has it, and `:` as `/`, as
/// macOS swaps them; in UTF-16, at most 255 units.
fn units(name: &str) -> io::Result<Vec<u16>> {
  let mut out: Vec<u16> = vec![];
  for c in name.chars() {
    let code = c as u32;
    match code {
      // Hangul syllables decompose by Unicode's formula.
      0xAC00..=0xD7A3 => {
        let s = code - 0xAC00;
        out.push((0x1100 + s / 588) as u16);
        out.push((0x1161 + s % 588 / 28) as u16);
        if !s.is_multiple_of(28) {
          out.push((0x11A7 + s % 28) as u16);
        }
      }
      0x3A => out.push(0x2F),
      0..=0xFFFF => match decomposed(code as u16) {
        Some(parts) => out.extend(parts.iter().take_while(|&&u| u != 0)),
        None => out.push(code as u16),
      },
      _ => out.extend(c.encode_utf16(&mut [0; 2]).iter()),
    }
  }

  // Combining marks in a row go in the order of their classes, an equal one's kept.
  let mut i = 1;
  while i < out.len() {
    let class = combining(out[i]);
    if class != 0 && combining(out[i - 1]) > class {
      out.swap(i - 1, i);
      i = i.saturating_sub(1).max(1);
    } else {
      i += 1;
    }
  }

  if out.len() > 255 {
    return Err(bad(format!("{name}: over 255 UTF-16 units, the longest HFS+ name")));
  }

  Ok(out)
}

/// `name` decomposed as [units] does, `:` kept: what Finder's `.DS_Store` names a file by.
pub fn nfd(name: &str) -> io::Result<String> {
  let units: Vec<u16> =
    units(name)?.into_iter().map(|u| if u == 0x2F { 0x3A } else { u }).collect();
  String::from_utf16(&units).map_err(|e| bad(format!("{name}: {e}")))
}

/// The decomposition of `c` in [NFD], 0 after its last unit.
fn decomposed(c: u16) -> Option<&'static [u16]> {
  row(&NFD, 5, c)
}

/// The combining class of `c` in [CCC], else 0.
fn combining(c: u16) -> u16 {
  row(&CCC, 2, c).map_or(0, |row| row[0])
}

/// The row of `c` in `table`, of rows `width` long sorted by their first unit, without it.
fn row(table: &'static [u16], width: usize, c: u16) -> Option<&'static [u16]> {
  let (mut lo, mut hi) = (0, table.len() / width);
  while lo < hi {
    let mid = (lo + hi) / 2;
    match table[mid * width].cmp(&c) {
      Ordering::Less => lo = mid + 1,
      Ordering::Greater => hi = mid,
      Ordering::Equal => return Some(&table[mid * width + 1..(mid + 1) * width]),
    }
  }

  None
}

/// `c` as HFS+ compares names, case folded (TN1150, "Case-Insensitive String Comparison
/// Algorithm"): 0 for a character it skips. A character that decomposes never reaches it, so a
/// range folds whole when its decomposable characters would fold to themselves.
fn fold(c: u16) -> u16 {
  match c {
    0 => 0xFFFF,
    0x41..=0x5A | 0xC0..=0xD6 | 0xD8..=0xDE => c + 0x20,
    // The letters of Latin Extended-A and -B that don't decompose, by their small letters.
    0x0110 | 0x0126 | 0x0132 | 0x013F | 0x0141 | 0x014A | 0x0152 | 0x0166 => c + 1,
    0x0182 | 0x0184 | 0x0187 | 0x018B | 0x0191 | 0x0198 | 0x01A2 | 0x01A4 | 0x01A7 | 0x01AC => {
      c + 1
    }
    0x01B3 | 0x01B5 | 0x01B8 | 0x01BC | 0x01E4 => c + 1,
    0x01C4 | 0x01C7 | 0x01CA | 0x01F1 => c + 2,
    0x01C5 | 0x01C8 | 0x01CB | 0x01F2 => c + 1,
    0x0181 => 0x0253,
    0x0186 => 0x0254,
    0x0189 => 0x0256,
    0x018A => 0x0257,
    0x018E => 0x01DD,
    0x018F => 0x0259,
    0x0190 => 0x025B,
    0x0193 => 0x0260,
    0x0194 => 0x0263,
    0x0196 => 0x0269,
    0x0197 => 0x0268,
    0x019C => 0x026F,
    0x019D => 0x0272,
    0x019F => 0x0275,
    0x01A9 => 0x0283,
    0x01AE => 0x0288,
    0x01B1 => 0x028A,
    0x01B2 => 0x028B,
    0x01B7 => 0x0292,
    0x0391..=0x03A1 | 0x03A3..=0x03A9 => c + 0x20,
    0x03E2..=0x03EE if c.is_multiple_of(2) => c + 1,
    0x0400..=0x040F => c + 0x50,
    0x0410..=0x042F => c + 0x20,
    0x0460..=0x0480 | 0x0490..=0x04BE if c.is_multiple_of(2) => c + 1,
    0x04C3 | 0x04C7 | 0x04CB => c + 1,
    0x0531..=0x0556 => c + 0x30,
    0x10A0..=0x10C5 => c + 0x30,
    0x200C..=0x200F | 0x202A..=0x202E | 0x206A..=0x206F | 0xFEFF => 0,
    0x2160..=0x216F => c + 0x10,
    0xFF21..=0xFF3A => c + 0x20,
    _ => c,
  }
}

/// `name` as HFS+'s catalog sorts it: its folded characters, those it skips left out.
fn folded(name: &[u16]) -> Vec<u16> {
  name.iter().map(|&c| fold(c)).filter(|&c| c != 0).collect()
}

// MARK: Volume

/// An entry of the tree flattened: its id, its parent's, its name in HFS+'s units.
struct Item<'a> {
  id: u32,
  parent: u32,
  name: Vec<u16>,
  entry: &'a Entry,
  /// A fork's first block and its length in bytes: a file's data, a link's target.
  fork: (u32, u64),
}

/// The bytes of an HFS+ volume holding the folder `root`, named as it is. The files on disk are
/// read straight into it.
///
/// Its blocks: the header's, the extents tree's, the forks one after another, the catalog, the
/// allocation bitmap and the last block, so each fork's place is known before the catalog, which
/// records it, and the bitmap, whose size follows from all the rest.
pub fn volume(root: &Entry) -> io::Result<Vec<u8>> {
  if !matches!(root.kind, Kind::Dir(_)) {
    return Err(bad(format!("{}: the volume's root is not a folder", root.name)));
  }

  let mut items = vec![Item {
    id: ROOT,
    parent: ROOT_PARENT,
    name: units(&root.name)?,
    entry: root,
    fork: (0, 0),
  }];
  flatten(root, ROOT, &mut items)?;

  let blocks = |len: u64| len.div_ceil(BLOCK as u64) as u32;
  let extents_at = 1;
  let mut next = extents_at + 1;
  for it in &mut items {
    let len = fork_len(it.entry);
    it.fork = (if len == 0 { 0 } else { next }, len);
    next += blocks(len);
  }

  let catalog_file = catalog(&items)?;
  let (catalog_at, catalog_blocks) = (next, blocks(catalog_file.len() as u64));
  let bitmap_at = catalog_at + catalog_blocks;
  let mut bitmap = 1;
  let total = loop {
    let total = bitmap_at + bitmap + 1;
    match blocks((total as u64).div_ceil(8)) {
      need if need > bitmap => bitmap = need,
      _ => break total,
    }
  };

  let mut out = vec![0u8; total as usize * BLOCK];
  for it in &items {
    let at = it.fork.0 as usize * BLOCK;
    let to = &mut out[at..at + it.fork.1 as usize];
    match &it.entry.kind {
      Kind::Dir(_) => {}
      Kind::File(Data::Bytes(data)) => to.copy_from_slice(data),
      Kind::File(Data::Disk(path, _)) => fill(path, to)?,
      Kind::Link(target) => to.copy_from_slice(target.as_bytes()),
    }
  }

  let extents_file = tree_file(&Tree { max_key: 10, compare: 0, attributes: BIG_KEYS }, &[])?;
  let at = extents_at as usize * BLOCK;
  out[at..at + extents_file.len()].copy_from_slice(&extents_file);
  let at = catalog_at as usize * BLOCK;
  out[at..at + catalog_file.len()].copy_from_slice(&catalog_file);

  // Every block is in use: the header's, the special files', the forks' and the last.
  let at = bitmap_at as usize * BLOCK;
  for n in 0..total as usize {
    out[at + n / 8] |= 0x80 >> (n % 8);
  }

  // The volume's id, of all it holds but the headers, written below.
  let id = hmac_sha256::Hash::hash(&out);
  let files = items.iter().filter(|it| !matches!(it.entry.kind, Kind::Dir(_))).count();
  let folders = items.len() - files - 1;
  // TN1150 has the create date in local time; GMT, as all the others, keeps a build the same
  // wherever it runs.
  let time = hfs_time(items.iter().map(|it| it.entry.time).max().unwrap_or(0));
  let mut head = vec![];
  head.extend_from_slice(b"H+");
  head.extend_from_slice(&4u16.to_be_bytes());
  // Unmounted cleanly, last by Mac OS X with no journal.
  head.extend_from_slice(&(1u32 << 8).to_be_bytes());
  head.extend_from_slice(b"10.0");
  // No journal; created, modified, backed up and checked; the files and folders, the root not
  // counted; the block size, the blocks, none free, and where a search for one starts, a block
  // of the volume (fsck_hfs repairs one past it).
  let last = total - 1;
  for word in [0, time, time, 0, time, files as u32, folders as u32, BLOCK as u32, total, 0, last] {
    head.extend_from_slice(&word.to_be_bytes());
  }

  let next_id = items.iter().map(|it| it.id).max().unwrap_or(ROOT).max(FIRST_ID - 1) + 1;
  // The clump sizes, the next catalog id, one write.
  for word in [BLOCK as u32, BLOCK as u32, next_id, 1] {
    head.extend_from_slice(&word.to_be_bytes());
  }

  // Only MacRoman's encoding is used: every record's hint is 0.
  head.extend_from_slice(&1u64.to_be_bytes());
  // Finder's info, its last 8 bytes the volume's id.
  head.extend_from_slice(&[0; 24]);
  head.extend_from_slice(&id[..8]);
  head.extend_from_slice(&fork_data(bitmap as u64 * BLOCK as u64, bitmap_at, bitmap));
  head.extend_from_slice(&fork_data(BLOCK as u64, extents_at, 1));
  head.extend_from_slice(&fork_data(catalog_file.len() as u64, catalog_at, catalog_blocks));
  head.extend_from_slice(&[0; 160]);
  out[1024..1024 + head.len()].copy_from_slice(&head);
  let end = out.len();
  out[end - 1024..end - 1024 + head.len()].copy_from_slice(&head);
  Ok(out)
}

/// Reads the file `path` into `to`, which is as long as the file was when its tree was read.
fn fill(path: &Path, to: &mut [u8]) -> io::Result<()> {
  let at = at(path);
  let mut file = fs::File::open(path).map_err(&at)?;
  file.read_exact(to).map_err(at)?;
  if file.read(&mut [0])? != 0 {
    return Err(bad(format!("{}: changed while the volume was written", path.display())));
  }

  Ok(())
}

/// The entries under `dir`, of the id `id`, into `items` with new ids, each folder's after it.
fn flatten<'a>(dir: &'a Entry, id: u32, items: &mut Vec<Item<'a>>) -> io::Result<()> {
  let Kind::Dir(entries) = &dir.kind else { return Ok(()) };
  let mut names = vec![];
  for entry in entries {
    let name = units(&entry.name)?;
    if name.is_empty() {
      return Err(bad(format!("{}: an entry with no name", dir.name)));
    }

    names.push((folded(&name), &entry.name));
    // The root is the first item, the others count up from the first id.
    let child = FIRST_ID + items.len() as u32 - 1;
    items.push(Item { id: child, parent: id, name, entry, fork: (0, 0) });
    flatten(entry, child, items)?;
  }

  // Names that differ in case, in normalization or by characters HFS+ skips collide.
  names.sort();
  if let Some(twins) = names.windows(2).find(|w| w[0].0 == w[1].0) {
    return Err(bad(format!(
      "{}: {} and {} are one name to HFS+",
      dir.name, twins[0].1, twins[1].1
    )));
  }

  Ok(())
}

/// The length of `entry`'s data fork: a file's data, a link's target.
fn fork_len(entry: &Entry) -> u64 {
  match &entry.kind {
    Kind::Dir(_) => 0,
    Kind::File(Data::Bytes(data)) => data.len() as u64,
    Kind::File(Data::Disk(_, len)) => *len,
    Kind::Link(target) => target.len() as u64,
  }
}

/// The catalog file of `items`: each one's record and thread.
fn catalog(items: &[Item]) -> io::Result<Vec<u8>> {
  let mut records = vec![];
  for it in items {
    let folder = matches!(it.entry.kind, Kind::Dir(_));
    let mut thread = vec![];
    let kind = if folder { FOLDER_THREAD } else { FILE_THREAD };
    thread.extend_from_slice(&kind.to_be_bytes());
    thread.extend_from_slice(&[0, 0]);
    thread.extend_from_slice(&it.parent.to_be_bytes());
    thread.extend_from_slice(&string(&it.name));
    records.push(((it.id, vec![]), key(it.id, &[]), thread));
    records.push(((it.parent, it.name.clone()), key(it.parent, &it.name), record(it)));
  }

  records.sort_by_cached_key(|((parent, name), ..)| (*parent, folded(name)));
  let records: Vec<_> = records.into_iter().map(|(_, key, data)| (key, data)).collect();
  let tree = Tree { max_key: 516, compare: 0, attributes: BIG_KEYS | VARIABLE_INDEX_KEYS };
  tree_file(&tree, &records)
}

/// A catalog key: its length, the parent's id and the name.
fn key(parent: u32, name: &[u16]) -> Vec<u8> {
  let mut out = ((6 + 2 * name.len()) as u16).to_be_bytes().to_vec();
  out.extend_from_slice(&parent.to_be_bytes());
  out.extend_from_slice(&string(name));
  out
}

/// `HFSUniStr255`: the length, then the units.
fn string(name: &[u16]) -> Vec<u8> {
  let mut out = (name.len() as u16).to_be_bytes().to_vec();
  name.iter().for_each(|u| out.extend_from_slice(&u.to_be_bytes()));
  out
}

/// The folder or file record of `it`.
fn record(it: &Item) -> Vec<u8> {
  let e = it.entry;
  let time = hfs_time(e.time);
  let mut out = vec![];
  let (kind, file_type) = match &e.kind {
    Kind::Dir(_) => (FOLDER, S_IFDIR),
    Kind::File(_) => (FILE, S_IFREG),
    Kind::Link(_) => (FILE, S_IFLNK),
  };

  out.extend_from_slice(&kind.to_be_bytes());
  match &e.kind {
    Kind::Dir(entries) => {
      out.extend_from_slice(&0u16.to_be_bytes());
      out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    }
    // Has a thread record.
    _ => out.extend_from_slice(&[0, 2, 0, 0, 0, 0]),
  }

  out.extend_from_slice(&it.id.to_be_bytes());
  // Created, content and attributes modified, accessed, backed up.
  for word in [time, time, time, time, 0] {
    out.extend_from_slice(&word.to_be_bytes());
  }

  // HFSPlusBSDInfo: root and wheel, no flags, the mode, a link count of 1 for a file.
  out.extend_from_slice(&[0; 10]);
  out.extend_from_slice(&(file_type | e.mode).to_be_bytes());
  out.extend_from_slice(&(if kind == FILE { 1u32 } else { 0 }).to_be_bytes());
  // Finder's info: a link is of type `slnk` by `rhap`.
  let mut info = [0u8; 32];
  if let Kind::Link(_) = e.kind {
    info[..8].copy_from_slice(b"slnkrhap");
  }

  out.extend_from_slice(&info);
  // The text encoding, MacRoman, and a reserved word.
  out.extend_from_slice(&[0; 8]);
  if kind == FILE {
    let blocks = it.fork.1.div_ceil(BLOCK as u64) as u32;
    out.extend_from_slice(&fork_data(it.fork.1, it.fork.0, blocks));
    out.extend_from_slice(&fork_data(0, 0, 0));
  }

  out
}

/// `HFSPlusForkData` of one extent: `size` bytes in `count` blocks from `start`.
fn fork_data(size: u64, start: u32, count: u32) -> Vec<u8> {
  let mut out = size.to_be_bytes().to_vec();
  out.extend_from_slice(&0u32.to_be_bytes());
  out.extend_from_slice(&count.to_be_bytes());
  out.extend_from_slice(&start.to_be_bytes());
  out.extend_from_slice(&count.to_be_bytes());
  out.resize(80, 0);
  out
}

/// `unix`, seconds since 1970, as an HFS+ date, which ends in 2040.
fn hfs_time(unix: u64) -> u32 {
  (unix + EPOCH).min(u32::MAX as u64) as u32
}

// MARK: B-trees

/// The node size of both trees: TN1150's minimum for the catalog, a block of the volume.
const NODE: usize = 4096;

/// A node descriptor's size, and the header node's records: the header, the user data and the map.
const DESCRIPTOR: usize = 14;
const HEADER: usize = 106;
const USER: usize = 128;
const MAP: usize = NODE - DESCRIPTOR - HEADER - USER - 8;

/// The node kinds of a descriptor.
const LEAF: u8 = 0xFF;
const INDEX: u8 = 0;
const HEAD: u8 = 1;

/// `BTHeaderRec.attributes`: a key's length is a u16, and index nodes keep the keys as they are.
const BIG_KEYS: u32 = 2;
const VARIABLE_INDEX_KEYS: u32 = 4;

/// What sets one tree apart from another.
struct Tree {
  /// The longest key, its length field not counted.
  pub max_key: u16,
  /// `BTHeaderRec.keyCompareType`.
  pub compare: u8,
  /// `BTHeaderRec.attributes`.
  pub attributes: u32,
}

/// The file of a tree of `records`, each its key (the length first) and its data, in key order.
/// An index record holds its child's first key as it is, of its own length, so the tree must be
/// one of [VARIABLE_INDEX_KEYS], as the catalog is; the extents tree has no index nodes here.
fn tree_file(tree: &Tree, records: &[(Vec<u8>, Vec<u8>)]) -> io::Result<Vec<u8>> {
  // A level's nodes, each its records; the leaves first. A level of one node is the root.
  let mut levels: Vec<Vec<Vec<Vec<u8>>>> = vec![];
  let leaves = records.iter().map(|(key, data)| [key.as_slice(), data].concat());
  let mut level = pack(leaves.collect(), 0);
  while !level.is_empty() {
    let root = level.len() == 1;
    // An index record is the first key of a node below, then its number, added below.
    let keys = level.iter().map(|node| record_key(&node[0]).to_vec()).collect();
    levels.push(level);
    if root {
      break;
    }

    level = pack(keys, 4);
  }

  let count = 1 + levels.iter().map(Vec::len).sum::<usize>();
  if count > MAP * 8 {
    return Err(bad(format!("a B-tree of {count} nodes, over the header's map")));
  }

  // Node numbers: the header 0, then the leaves, then each level above in turn.
  let mut first = vec![];
  let mut next = 1;
  for level in &levels {
    first.push(next);
    next += level.len();
  }

  let mut out = vec![0; count * NODE];
  for (depth, level) in levels.iter().enumerate() {
    let (kind, height) = (if depth == 0 { LEAF } else { INDEX }, depth as u8 + 1);
    for (i, records) in level.iter().enumerate() {
      let number = first[depth] + i;
      let mut records = records.clone();
      if depth > 0 {
        // Each record points at its node below, the one at the same place in that level.
        for (j, rec) in records.iter_mut().enumerate() {
          let below = first[depth - 1] + index_of(&levels[depth], i, j);
          rec.extend_from_slice(&(below as u32).to_be_bytes());
        }
      }

      let flink = if i + 1 < level.len() { number + 1 } else { 0 };
      let blink = if i > 0 { number - 1 } else { 0 };
      let node = node(flink as u32, blink as u32, kind, height, &records);
      out[number * NODE..(number + 1) * NODE].copy_from_slice(&node);
    }
  }

  let leaves = levels.first().map_or(0, Vec::len);
  let mut head = vec![];
  head.extend_from_slice(&(levels.len() as u16).to_be_bytes());
  let root = if levels.is_empty() { 0 } else { count - 1 };
  let (first_leaf, last_leaf) = if leaves == 0 { (0, 0) } else { (1, leaves) };
  for word in [root, records.len(), first_leaf, last_leaf] {
    head.extend_from_slice(&(word as u32).to_be_bytes());
  }

  head.extend_from_slice(&(NODE as u16).to_be_bytes());
  head.extend_from_slice(&tree.max_key.to_be_bytes());
  // All nodes, none free.
  head.extend_from_slice(&(count as u32).to_be_bytes());
  head.extend_from_slice(&0u32.to_be_bytes());
  head.extend_from_slice(&0u16.to_be_bytes());
  head.extend_from_slice(&(NODE as u32).to_be_bytes());
  head.push(0);
  head.push(tree.compare);
  head.extend_from_slice(&tree.attributes.to_be_bytes());
  head.resize(HEADER, 0);

  let mut map = vec![0u8; MAP];
  for n in 0..count {
    map[n / 8] |= 0x80 >> (n % 8);
  }

  let header = node(0, 0, HEAD, 0, &[head, vec![0; USER], map]);
  out[..NODE].copy_from_slice(&header);
  Ok(out)
}

/// The key of `record`, its length field with it.
fn record_key(record: &[u8]) -> &[u8] {
  let len = u16::from_be_bytes([record[0], record[1]]) as usize;
  &record[..2 + len]
}

/// The place in the level below of the node record `j` of node `i` points at: the records of a
/// level, in order, are the nodes below.
fn index_of(level: &[Vec<Vec<u8>>], i: usize, j: usize) -> usize {
  level[..i].iter().map(Vec::len).sum::<usize>() + j
}

/// `records` packed into nodes in order, each as full as it takes, `extra` bytes kept for each:
/// an index record's pointer, added after.
fn pack(records: Vec<Vec<u8>>, extra: usize) -> Vec<Vec<Vec<u8>>> {
  let mut nodes: Vec<Vec<Vec<u8>>> = vec![];
  let mut used = NODE;
  for rec in records {
    // Its bytes and its offset.
    let size = rec.len() + extra + 2;
    if used + size + 2 > NODE {
      nodes.push(vec![]);
      used = DESCRIPTOR;
    }

    used += size;
    if let Some(node) = nodes.last_mut() {
      node.push(rec);
    }
  }

  nodes
}

/// A node of `records`: the descriptor, the records, and their offsets from the end back, then the
/// free space's.
fn node(flink: u32, blink: u32, kind: u8, height: u8, records: &[Vec<u8>]) -> Vec<u8> {
  let mut out = vec![0; NODE];
  out[0..4].copy_from_slice(&flink.to_be_bytes());
  out[4..8].copy_from_slice(&blink.to_be_bytes());
  out[8] = kind;
  out[9] = height;
  out[10..12].copy_from_slice(&(records.len() as u16).to_be_bytes());
  let mut at = DESCRIPTOR;
  for (i, rec) in records.iter().enumerate() {
    out[NODE - 2 * (i + 1)..NODE - 2 * i].copy_from_slice(&(at as u16).to_be_bytes());
    out[at..at + rec.len()].copy_from_slice(rec);
    at += rec.len();
  }

  let free = NODE - 2 * (records.len() + 1);
  out[free..free + 2].copy_from_slice(&(at as u16).to_be_bytes());
  out
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use crate::tool::get;

  /// The order of two names under one folder, as HFS+'s catalog sorts them.
  fn compare(a: &[u16], b: &[u16]) -> Ordering {
    folded(a).cmp(&folded(b))
  }

  #[test]
  fn names() {
    let cases = [
      ("Hello.app", "Hello.app"),
      ("Wavö Café", "Wavo\u{308} Cafe\u{301}"),
      ("a:b", "a/b"),
      ("한", "\u{1112}\u{1161}\u{11AB}"),
      ("ǖ", "u\u{308}\u{304}"),
      // A dot below (220) goes before a grave (230), wherever they came from.
      ("à\u{323}", "a\u{323}\u{300}"),
      ("Ω", "Ω"),
      ("\u{2126}", "\u{2126}"),
      ("😀", "😀"),
    ];

    for (name, want) in cases {
      let want: Vec<u16> = want.encode_utf16().collect();
      assert_eq!(units(name).unwrap(), want, "{name}");
    }

    assert_eq!(nfd("Wavö:1").unwrap(), "Wavo\u{308}:1");
    assert!(units(&"x".repeat(256)).is_err());
    assert!(units(&"x".repeat(255)).is_ok());
  }

  #[test]
  fn modes_off_unix() {
    let programs: [(&[u8], bool); 6] = [
      (&[0xCF, 0xFA, 0xED, 0xFE, 7], true),
      (&[0xCA, 0xFE, 0xBA, 0xBE], true),
      (&[0xCA, 0xFE, 0xBA, 0xBF], true),
      (b"#!/bin/sh", true),
      (b"<?xml", false),
      (&[0xCF, 0xFA], false),
    ];

    for (head, want) in programs {
      assert_eq!(program(head), want, "{head:?}");
    }

    let tmp = crate::tool::Temp::new().unwrap();
    fs::write(tmp.join("exe"), [0xCF, 0xFA, 0xED, 0xFE, 7]).unwrap();
    fs::write(tmp.join("plist"), "<?xml").unwrap();
    let disk = |name: &str| Kind::File(Data::Disk(tmp.join(name), 5));
    assert_eq!(off_unix(&disk("exe")), 0o755);
    assert_eq!(off_unix(&disk("plist")), 0o644);
    assert_eq!(off_unix(&Kind::File(Data::Bytes(vec![0xCF, 0xFA, 0xED, 0xFE]))), 0o644);
    assert_eq!(off_unix(&Kind::Dir(vec![])), 0o755);
    assert_eq!(off_unix(&Kind::Link("x".into())), 0o755);
  }

  #[test]
  fn order() {
    let of = |s: &str| units(s).unwrap();
    assert_eq!(compare(&of("ABC"), &of("abc")), Ordering::Equal);
    assert_eq!(compare(&of("a"), &of("B")), Ordering::Less);
    assert_eq!(compare(&of("ab"), &of("a")), Ordering::Greater);
    assert_eq!(compare(&of("Ä"), &of("äb")), Ordering::Less);
    assert_eq!(compare(&of("ПРИВЕТ"), &of("привет")), Ordering::Equal);
    assert_eq!(compare(&of("a\u{200D}b"), &of("AB")), Ordering::Equal);
    // `[` is between the capitals and the small letters, which they fold to.
    assert_eq!(compare(&of("["), &of("A")), Ordering::Less);
  }

  #[test]
  fn errors() {
    let entry = |name: &str, kind| Entry { name: name.into(), mode: 0o755, time: 0, kind };
    let file = |name: &str| entry(name, Kind::File(Data::Bytes(vec![])));
    let twins = [
      ("a", "A", "A and a"),
      ("é", "e\u{301}", "e\u{301} and é"),
      ("ab", "a\u{200D}b", "ab and a\u{200D}b"),
    ];
    for (one, two, both) in twins {
      let e = volume(&entry("V", Kind::Dir(vec![file(one), file(two)]))).unwrap_err().to_string();
      assert_eq!(e, format!("V: {both} are one name to HFS+"));
    }

    let e = volume(&file("V")).unwrap_err().to_string();
    assert_eq!(e, "V: the volume's root is not a folder");
  }

  /// A FIFO in the tree is refused before it is read, which would block.
  #[cfg(unix)]
  #[test]
  fn fifo() {
    let tmp = crate::tool::Temp::new().unwrap();
    fs::create_dir(tmp.join("app")).unwrap();
    let fifo = tmp.join("app/pipe");
    crate::tool::run(std::process::Command::new("mkfifo").arg(&fifo)).unwrap();
    let e = Entry::read(&tmp.join("app")).unwrap_err().to_string();
    assert_eq!(e, format!("{}: not a file, folder or symlink", fifo.display()));
  }

  /// A tree made under a `umask` of 077 reads as the writer's modes, a link keeping its own, each
  /// folder's entries by name.
  #[cfg(unix)]
  #[test]
  fn modes() {
    use std::os::unix::fs::MetadataExt;

    let tmp = crate::tool::Temp::new().unwrap();
    let app = tmp.join("app");
    let modes =
      [("bin", 0o700), ("data", 0o600), ("group", 0o750), ("odd", 0o4711), ("dir", 0o700)];
    fs::create_dir(&app).unwrap();
    for (name, mode) in modes {
      match name {
        "dir" => fs::create_dir(app.join(name)).unwrap(),
        _ => fs::write(app.join(name), name).unwrap(),
      }

      crate::tool::set_mode(&app.join(name), mode).unwrap();
    }

    std::os::unix::fs::symlink("data", app.join("link")).unwrap();
    crate::tool::set_mode(&app, 0o700).unwrap();
    let root = Entry::read(&app).unwrap();
    crate::tool::set_mode(&app, 0o755).unwrap();
    let link = fs::symlink_metadata(app.join("link")).unwrap().mode() as u16 & 0o7777;
    let Kind::Dir(entries) = &root.kind else { panic!("{root:?}") };
    let got: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.mode)).collect();
    let want = [
      ("bin", 0o755),
      ("data", 0o644),
      ("dir", 0o755),
      ("group", 0o755),
      ("link", link),
      ("odd", 0o755),
    ];
    assert_eq!(got, want);
    assert_eq!(root.mode, 0o755);
  }

  /// The volume's id is of all it holds: another file's data gives another.
  #[test]
  fn ids() {
    let id = |data: &[u8]| {
      let file = Entry {
        name: "f".into(),
        mode: 0o644,
        time: 0,
        kind: Kind::File(Data::Bytes(data.to_vec())),
      };
      let root = Entry { name: "V".into(), mode: 0o755, time: 0, kind: Kind::Dir(vec![file]) };
      volume(&root).unwrap()[1024 + 104..1024 + 112].to_vec()
    };

    assert_eq!(id(b"one"), id(b"one"));
    assert_ne!(id(b"one"), id(b"two"));
  }

  /// A test bundle on disk at `dir/<name>`: an executable, a framework's links, files empty and
  /// over a block, non-ASCII names, and enough files for a catalog of three levels.
  #[cfg(unix)]
  pub fn bundle(dir: &Path, name: &str) -> PathBuf {
    let app = dir.join(name);
    let write = |path: &str, data: &[u8], mode: u32| {
      let path = app.join(path);
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      fs::write(&path, data).unwrap();
      crate::tool::set_mode(&path, mode).unwrap();
    };

    let noise: Vec<u8> =
      (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    write("Contents/MacOS/hello", &noise[..10_000], 0o755);
    write("Contents/Info.plist", b"<plist/>", 0o644);
    write("Contents/Resources/empty", b"", 0o600);
    write("Contents/Resources/Café ñ.txt", "Grüße".as_bytes(), 0o644);
    write("Contents/Resources/big.bin", &noise, 0o644);
    write("Contents/Frameworks/X.framework/Versions/A/X", b"x", 0o755);
    for i in 0..1500 {
      write(&format!("Contents/Resources/many/file-{i:04}.txt"), format!("{i}").as_bytes(), 0o644);
    }

    let framework = app.join("Contents/Frameworks/X.framework");
    std::os::unix::fs::symlink("A", framework.join("Versions/Current")).unwrap();
    std::os::unix::fs::symlink("Versions/Current/X", framework.join("X")).unwrap();
    app
  }

  /// A folder of one file per character HFS+ folds or might, but those it folds alike, so the
  /// catalog's order of them is checked by fsck_hfs.
  #[cfg(target_os = "macos")]
  pub fn folds() -> Entry {
    let ranges = [0x20..0x250, 0x370..0x400, 0x400..0x530, 0x531..0x590, 0x10A0..0x1100];
    let more = [0x2160..0x2180, 0xFF21..0xFF5B, 0x200B..0x2010, 0xFEFF..0xFF00];
    let mut names = std::collections::HashSet::new();
    let mut entries = vec![];
    for c in ranges.into_iter().chain(more).flatten() {
      let Some(ch) = char::from_u32(c) else { continue };
      if ch == '/' || ch == ':' || ch.is_control() {
        continue;
      }

      let name = format!("x{ch}");
      let of = units(&name).unwrap();
      if of.len() != 2 || !names.insert(folded(&of)) {
        continue;
      }

      entries.push(Entry { name, mode: 0o644, time: 0, kind: Kind::File(Data::Bytes(vec![])) });
    }

    Entry { name: "folds".into(), mode: 0o755, time: 0, kind: Kind::Dir(entries) }
  }

  /// Checks the tree at `path` is `entry`: the names as HFS+ keeps them, the modes, times, data
  /// and links.
  #[cfg(target_os = "macos")]
  pub fn same(entry: &Entry, path: &Path) {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).unwrap();
    assert_eq!(meta.mode() as u16 & 0o7777, entry.mode, "{}", path.display());
    match &entry.kind {
      Kind::Dir(entries) => {
        let mut names: Vec<String> = fs::read_dir(path)
          .unwrap()
          .map(|e| e.unwrap().file_name().into_string().unwrap())
          .collect();
        let mut want: Vec<String> = entries.iter().map(|e| nfd(&e.name).unwrap()).collect();
        names.sort();
        want.sort();
        assert_eq!(names, want, "{}", path.display());
        for e in entries {
          same(e, &path.join(nfd(&e.name).unwrap()));
        }
      }
      Kind::File(data) => {
        let want = match data {
          Data::Bytes(data) => data.clone(),
          Data::Disk(from, _) => fs::read(from).unwrap(),
        };

        assert_eq!(fs::read(path).unwrap(), want, "{}", path.display());
        assert_eq!(meta.mtime() as u64, entry.time, "{}", path.display());
      }
      Kind::Link(target) => {
        assert_eq!(fs::read_link(path).unwrap().to_str(), Some(target.as_str()))
      }
    }
  }

  /// A disk image hdiutil attached, unseen by Finder, detached on drop.
  #[cfg(target_os = "macos")]
  pub struct Attached {
    pub dev: String,
  }

  #[cfg(target_os = "macos")]
  impl Attached {
    /// `image` attached read-only, a raw image if `raw`, at `mount` or with nothing mounted.
    pub fn new(image: &Path, raw: bool, mount: Option<&Path>) -> Self {
      use std::process::Command;

      let mut cmd = Command::new("hdiutil");
      cmd.args(["attach", "-readonly", "-nobrowse", "-noautoopen"]);
      if raw {
        cmd.args(["-imagekey", "diskimage-class=CRawDiskImage"]);
      }

      match mount {
        Some(dir) => cmd.arg("-mountpoint").arg(dir),
        None => cmd.arg("-nomount"),
      };

      let out = crate::tool::run(cmd.arg(image)).unwrap();
      let out = String::from_utf8(out).unwrap();
      let dev = out.split_whitespace().find(|w| w.starts_with("/dev/disk")).unwrap();
      Self { dev: dev.into() }
    }

    /// `fsck_hfs -n` of the device, its output; fails the test if it finds a fault.
    pub fn fsck(&self) -> String {
      let out = std::process::Command::new("fsck_hfs").args(["-n", &self.dev]).output().unwrap();
      let said =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
      assert!(out.status.success(), "{said}");
      said
    }
  }

  #[cfg(target_os = "macos")]
  impl Drop for Attached {
    fn drop(&mut self) {
      use std::process::Command;

      let detach = |force: bool| {
        let mut cmd = Command::new("hdiutil");
        cmd.args(["detach", &self.dev]);
        if force {
          cmd.arg("-force");
        }

        cmd.output().is_ok_and(|o| o.status.success())
      };

      if !detach(false) {
        detach(true);
      }
    }
  }

  /// A volume of a test bundle, the fold folder, a link to /Applications and a hidden file,
  /// attached as a raw disk: fsck_hfs finds no fault, and mounted it reads back as written.
  #[cfg(target_os = "macos")]
  #[test]
  fn read_by_macos() {
    let tmp = crate::tool::Temp::new().unwrap();
    let app = bundle(&tmp, "Wavö Test.app");
    let mut root = Entry::read(&app).unwrap();
    root.name = "Wavö Test".into();
    let link = Entry {
      name: "Applications".into(),
      mode: 0o755,
      time: 0,
      kind: Kind::Link("/Applications".into()),
    };
    let hidden = Entry {
      name: ".DS_Store".into(),
      mode: 0o644,
      time: 0,
      kind: Kind::File(Data::Bytes(vec![7; 9000])),
    };
    let Kind::Dir(top) = &mut root.kind else { panic!() };
    top.extend([folds(), link, hidden]);

    let image = tmp.join("test.img");
    fs::write(&image, volume(&root).unwrap()).unwrap();
    let said = Attached::new(&image, true, None).fsck();
    assert!(said.contains("appears to be OK"), "{said}");

    let mount = tmp.join("mnt");
    fs::create_dir(&mount).unwrap();
    let attached = Attached::new(&image, true, Some(&mount));
    same(&root, &mount);
    drop(attached);
  }

  fn word(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(get(data, at).unwrap())
  }

  fn half(data: &[u8], at: usize) -> usize {
    u16::from_be_bytes(get(data, at).unwrap()) as usize
  }

  /// The records of node `n` of `file`.
  fn records(file: &[u8], n: usize) -> Vec<Vec<u8>> {
    let node = &file[n * NODE..(n + 1) * NODE];
    let count = half(node, 10);
    (0..count)
      .map(|i| {
        let (start, end) = (half(node, NODE - 2 * (i + 1)), half(node, NODE - 2 * (i + 2)));
        node[start..end].to_vec()
      })
      .collect()
  }

  /// A leaf record of the key `n` and 400 bytes after it, and 8 bytes of data.
  fn record(n: u32) -> (Vec<u8>, Vec<u8>) {
    let mut key = 404u16.to_be_bytes().to_vec();
    key.extend_from_slice(&n.to_be_bytes());
    key.resize(406, 0);
    (key, vec![n as u8; 8])
  }

  const TREE: Tree = Tree { max_key: 404, compare: 0, attributes: BIG_KEYS | VARIABLE_INDEX_KEYS };

  #[test]
  fn empty() {
    let file = tree_file(&TREE, &[]).unwrap();
    assert_eq!(file.len(), NODE);
    assert_eq!(file[8], HEAD);
    // Depth 0, root 0, no records or leaves, the node size, one node, none free.
    assert_eq!(half(&file, 14), 0);
    assert_eq!([word(&file, 16), word(&file, 20), word(&file, 24)], [0, 0, 0]);
    assert_eq!(half(&file, 14 + 18), NODE);
    assert_eq!([word(&file, 14 + 22), word(&file, 14 + 26)], [1, 0]);
    assert_eq!(records(&file, 0).iter().map(Vec::len).collect::<Vec<_>>(), [106, 128, MAP]);
    assert_eq!(records(&file, 0)[2][0], 0x80);
  }

  /// Enough records for four levels: each leaf, then each index node, is linked to its
  /// neighbours, and the index records name the first key of the node they point at.
  #[test]
  fn levels() {
    let all: Vec<_> = (0..3000).map(record).collect();
    let file = tree_file(&TREE, &all).unwrap();
    let count = file.len() / NODE;
    let depth = half(&file, 14);
    let root = word(&file, 16) as usize;
    assert_eq!(depth, 4);
    assert_eq!(root, count - 1);
    assert_eq!(word(&file, 20), 3000);
    assert_eq!(word(&file, 14 + 22) as usize, count);

    // Walk down the leftmost path, then along each level.
    let mut n = root;
    for height in (1..=depth).rev() {
      assert_eq!(file[n * NODE + 9] as usize, height);
      assert_eq!(word(&file, n * NODE + 4), 0);
      let mut along = n;
      let mut seen = vec![];
      while along != 0 {
        let node = &file[along * NODE..];
        for rec in records(&file, along) {
          seen.push(word(&rec, 2));
          if height > 1 {
            let child = word(&rec, rec.len() - 4) as usize;
            assert_eq!(records(&file, child)[0][..406], rec[..406]);
          }
        }

        along = word(node, 0) as usize;
      }

      assert!(seen.windows(2).all(|w| w[0] < w[1]));
      if height == 1 {
        assert_eq!(seen, (0..3000).collect::<Vec<_>>());
        assert_eq!(word(&file, 24) as usize, n);
      }

      let first = records(&file, n)[0].clone();
      n = word(&first, first.len() - 4) as usize;
    }
  }
}
