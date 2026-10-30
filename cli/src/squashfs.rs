// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The squashfs 4.0 image of a [Tree], as an AppImage carries its AppDir, by the format the
//! kernel's `Documentation/filesystems/squashfs.rst` and its `squashfs_fs.h` give: the superblock,
//! the files' data in 128 KiB zlib blocks, the inode and directory tables, and the id table. It has
//! no fragments, no export table and no xattrs; everything is owned by root.

use std::collections::HashMap;
use std::io::{self, Write};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::bad;
use crate::tree::{Node, Tree};

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
pub fn build(tree: &Tree, mtime: u32) -> io::Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
  use std::io::Read;

  use flate2::read::ZlibDecoder;

  use super::*;

  fn u16le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
  }

  fn u32le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
  }

  fn u64le(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
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
  struct Image<'a> {
    img: &'a [u8],
    inodes: (Vec<u8>, HashMap<u32, usize>),
    dirs: (Vec<u8>, HashMap<u32, usize>),
  }

  impl Image<'_> {
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

    let img = build(&tree, 1_700_000_000).unwrap();
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

    let image = Image {
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
    assert!(build(&tree, 0).is_ok());
    tree.file(&"x".repeat(256), 0o644, "");
    let e = build(&tree, 0).unwrap_err().to_string();
    assert!(e.ends_with("squashfs holds names of 1 to 255 bytes"), "{e}");
  }
}
