// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A 64-bit Windows exe's resources: the resource section built from a tree of them, the tree read
//! back from an exe, and an exe given a new one, so no Windows tool is needed. The layout is
//! Microsoft's "PE Format" (learn.microsoft.com/windows/win32/debug/pe-format).

use std::collections::BTreeMap;
use std::io;

use crate::bad;

pub const RT_ICON: u16 = 3;
pub const RT_GROUP_ICON: u16 = 14;
pub const RT_VERSION: u16 = 16;
pub const RT_MANIFEST: u16 = 24;
/// en-US, rc.exe's language.
pub const LANG: u16 = 0x409;
/// The COFF header's machines of x64 and arm64.
pub const AMD64: u16 = 0x8664;
pub const ARM64: u16 = 0xAA64;

/// Resources by type, id and language, all numbered: the packer names none by a string.
pub type Tree = BTreeMap<u16, BTreeMap<u16, BTreeMap<u16, Vec<u8>>>>;

/// A section header's size.
const SECTION: usize = 40;
/// The data directories' offset in a PE32+ optional header; the resource directory is the third,
/// the certificate table the fifth.
const DIRS: usize = 112;
/// IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ.
const RSRC: u32 = 0x4000_0040;
/// A resource directory entry's offset with this bit is a subdirectory's.
const SUBDIR: u32 = 1 << 31;

/// The headers of a PE32+ file that the edit reads; offsets are into the file.
struct Pe {
  /// The COFF file header's, after `PE\0\0`.
  coff: usize,
  /// The optional header's.
  opt: usize,
  /// The section table's.
  table: usize,
  machine: u16,
  sections: Vec<Section>,
  section_align: u32,
  file_align: u32,
  image_size: u32,
  headers_size: u32,
}

struct Section {
  va: u32,
  vsize: u32,
  raw_size: u32,
  raw_at: u32,
}

impl Pe {
  fn read(data: &[u8]) -> io::Result<Self> {
    if data.get(..2) != Some(b"MZ") {
      return Err(bad("not an exe: no MZ header"));
    }

    let sig = u32_at(data, 0x3C)? as usize;
    if data.get(sig..sig + 4) != Some(b"PE\0\0") {
      return Err(bad("not an exe: no PE header"));
    }

    let coff = sig + 4;
    let (machine, count, opt_size) =
      (u16_at(data, coff)?, u16_at(data, coff + 2)?, u16_at(data, coff + 16)?);
    let opt = coff + 20;
    if u16_at(data, opt)? != 0x20B {
      return Err(bad("not a 64-bit exe (PE32+)"));
    }

    if (opt_size as usize) < DIRS + 8 * 5 || u32_at(data, opt + DIRS - 4)? < 5 {
      return Err(bad("its optional header has no resource or certificate directory"));
    }

    let table = opt + opt_size as usize;
    let section = |i: usize| {
      let at = table + SECTION * i;
      Ok(Section {
        vsize: u32_at(data, at + 8)?,
        va: u32_at(data, at + 12)?,
        raw_size: u32_at(data, at + 16)?,
        raw_at: u32_at(data, at + 20)?,
      })
    };

    let sections: Vec<Section> = (0..count as usize).map(section).collect::<io::Result<_>>()?;
    if sections.iter().any(|s| s.raw_at as u64 + s.raw_size as u64 > data.len() as u64) {
      return Err(bad("the exe ends inside its sections"));
    }

    let (section_align, file_align) = (u32_at(data, opt + 32)?, u32_at(data, opt + 36)?);
    // Windows' ranges for an exe; a driver's smaller section alignment is not one.
    let file_ok = file_align.is_power_of_two() && (512..=0x10000).contains(&file_align);
    if !file_ok || !section_align.is_power_of_two() || section_align < 4096 {
      return Err(bad(format!(
        "its file alignment {file_align:#x} or section alignment {section_align:#x} is not an \
         exe's: a power of two, 512 to 64K and at least 4K"
      )));
    }

    let (image_size, headers_size) = (u32_at(data, opt + 56)?, u32_at(data, opt + 60)?);
    Ok(Self {
      coff,
      opt,
      table,
      machine,
      sections,
      section_align,
      file_align,
      image_size,
      headers_size,
    })
  }

  /// The `len` bytes at `rva` in the file, all in one section's data.
  fn slice<'a>(&self, data: &'a [u8], rva: u32, len: u32) -> io::Result<&'a [u8]> {
    let (rva, len) = (rva as u64, len as u64);
    for s in &self.sections {
      let (va, size) = (s.va as u64, s.vsize.min(s.raw_size) as u64);
      if va <= rva && rva + len <= va + size {
        let at = (s.raw_at as u64 + rva - va) as usize;
        return data.get(at..at + len as usize).ok_or_else(short);
      }
    }

    Err(bad(format!("its resources point outside its sections' data, at {rva:#x}")))
  }

  /// The resources the resource directory points at but those of the types `skip` gives; none
  /// without one.
  fn resources(&self, data: &[u8], skip: impl Fn(u16) -> bool) -> io::Result<Tree> {
    let (rva, size) = (u32_at(data, self.opt + DIRS + 16)?, u32_at(data, self.opt + DIRS + 20)?);
    if rva == 0 || size == 0 {
      return Ok(Tree::new());
    }

    let dir = self.slice(data, rva, size)?;
    walk(dir, skip, data.len(), |rva, len| self.slice(data, rva, len))
  }
}

/// The resources of the resource directory `dir` but those of the types `skip` gives, their data
/// by `read` (an RVA and a length). A directory pointing into itself is an error, never an endless
/// walk: it visits at most as many entries as `dir` holds and `limit` bytes of data.
fn walk<'a>(
  dir: &[u8],
  skip: impl Fn(u16) -> bool,
  limit: usize,
  read: impl Fn(u32, u32) -> io::Result<&'a [u8]>,
) -> io::Result<Tree> {
  let (mut budget, mut bytes) = (dir.len() / 8, 0);
  let mut tree = Tree::new();
  for (kind, at) in entries(dir, 0, true, &mut budget)? {
    if skip(kind) {
      continue;
    }

    for (id, at) in entries(dir, at, true, &mut budget)? {
      for (lang, at) in entries(dir, at, false, &mut budget)? {
        let found = read(u32_at(dir, at)?, u32_at(dir, at + 4)?)?;
        bytes += found.len();
        if bytes > limit {
          return Err(bad("its resource directory points into itself"));
        }

        tree.entry(kind).or_default().entry(id).or_default().insert(lang, found.to_vec());
      }
    }
  }

  Ok(tree)
}

/// The ids in the resource directory at `at` in `dir` and where each points, a subdirectory if
/// `sub`, else a data entry; each entry takes one of `budget`.
fn entries(dir: &[u8], at: usize, sub: bool, budget: &mut usize) -> io::Result<Vec<(u16, usize)>> {
  if u16_at(dir, at + 12)? > 0 {
    return Err(bad("a resource named by a string, which the packer can't keep"));
  }

  let count = u16_at(dir, at + 14)? as usize;
  *budget =
    budget.checked_sub(count).ok_or_else(|| bad("its resource directory points into itself"))?;

  let entry = |i: usize| {
    let (id, to) = (u32_at(dir, at + 16 + 8 * i)?, u32_at(dir, at + 20 + 8 * i)?);
    match u16::try_from(id) {
      Ok(id) if (to & SUBDIR != 0) == sub => Ok((id, (to & !SUBDIR) as usize)),
      _ => Err(bad("its resource directory is malformed")),
    }
  };

  (0..count).map(entry).collect()
}

/// The resources of the PE32+ file `data`.
#[cfg_attr(not(test), expect(dead_code, reason = "the tests read the resources back"))]
pub fn resources(data: &[u8]) -> io::Result<Tree> {
  let pe = Pe::read(data)?;
  pe.resources(data, |_| false)
}

/// `data`, a PE32+ exe for `machine`, with a new last section, `.rsrc`, of the resources `ours`
/// and the exe's own of the other types, the resource directory pointing at it; the old ones stay
/// in the file, unused. Its checksum is set again, though Windows checks only a driver's.
pub fn with_resources(data: &[u8], machine: u16, ours: Tree) -> io::Result<Vec<u8>> {
  let pe = Pe::read(data)?;
  if pe.machine != machine {
    return Err(bad(format!("the exe is for {}, not {}", arch(pe.machine), arch(machine))));
  }

  // A new section breaks a signature: it covers the headers.
  if u32_at(data, pe.opt + DIRS + 36)? != 0 {
    return Err(bad("the exe is signed: its resources go in before its signature"));
  }

  // Only the types kept are read: one replaced may hold what the packer can't read, as
  // embed-resource's icon named `APP`.
  let mut tree = pe.resources(data, |kind| ours.contains_key(&kind))?;
  tree.extend(ours);

  // The new section header must fit before the first section's data, over zeros only.
  let header = pe.table + SECTION * pe.sections.len();
  let first = pe.sections.iter().map(|s| s.raw_at).filter(|&at| at > 0).min();
  let room = first.unwrap_or(u32::MAX).min(pe.headers_size) as usize;
  let free = data.get(header..header + SECTION).is_some_and(|b| b.iter().all(|&x| x == 0));
  if header + SECTION > room || !free {
    return Err(bad("its headers have no room for one more section header"));
  }

  let end = pe.sections.iter().map(|s| s.va as u64 + s.vsize as u64).max().unwrap_or(0);
  let va = align(end.max(pe.image_size as u64), pe.section_align as u64);
  let blob = rsrc(&tree, va as u32);
  let (at, raw) = (
    align(data.len() as u64, pe.file_align as u64),
    align(blob.len() as u64, pe.file_align as u64),
  );
  let image = align(va + blob.len() as u64, pe.section_align as u64);
  if image > u32::MAX as u64 || at + raw > u32::MAX as u64 {
    return Err(bad("the exe with its resources passes 4 GiB"));
  }

  let mut out = data.to_vec();
  out.resize(at as usize, 0);
  let fields = [blob.len() as u32, va as u32, raw as u32, at as u32, 0, 0, 0, RSRC];
  put(&mut out, header, b".rsrc\0\0\0");
  for (i, x) in fields.iter().enumerate() {
    // NumberOfRelocations and NumberOfLinenumbers are the 16-bit halves of the seventh field.
    put(&mut out, header + 8 + 4 * i, &x.to_le_bytes());
  }

  out.extend_from_slice(&blob);
  out.resize((at + raw) as usize, 0);
  put(&mut out, pe.coff + 2, &(pe.sections.len() as u16 + 1).to_le_bytes());
  let initialized = u32_at(&out, pe.opt + 8)?.saturating_add(raw as u32);
  put(&mut out, pe.opt + 8, &initialized.to_le_bytes());
  put(&mut out, pe.opt + 56, &(image as u32).to_le_bytes());
  put(&mut out, pe.opt + DIRS + 16, &(va as u32).to_le_bytes());
  put(&mut out, pe.opt + DIRS + 20, &(blob.len() as u32).to_le_bytes());
  put(&mut out, pe.opt + 64, &[0; 4]);
  let sum = checksum(&out);
  put(&mut out, pe.opt + 64, &sum.to_le_bytes());
  Ok(out)
}

/// The resource section of `tree` at `rva`: the root, a directory per type, one per id (its
/// languages), the data entries, then the data, each at 8 bytes.
fn rsrc(tree: &Tree, rva: u32) -> Vec<u8> {
  let dir = |n: usize| 16 + 8 * n;
  let ids = tree.values();
  let names: usize = ids.clone().map(|ids| dir(ids.len())).sum();
  let langs: usize = ids.clone().flat_map(|ids| ids.values()).map(|l| dir(l.len())).sum();
  let leaves = ids.flat_map(|ids| ids.values()).map(|l| l.len()).sum::<usize>();
  let entries = dir(tree.len()) + names + langs;
  let mut out = vec![0; align((entries + 16 * leaves) as u64, 8) as usize];

  let (mut sub, mut lang_dir, mut leaf) = (dir(tree.len()), dir(tree.len()) + names, 0);
  put(&mut out, 14, &(tree.len() as u16).to_le_bytes());
  for (i, (&kind, ids)) in tree.iter().enumerate() {
    entry(&mut out, 16 + 8 * i, kind, sub as u32 | SUBDIR);
    put(&mut out, sub + 14, &(ids.len() as u16).to_le_bytes());
    for (j, (&id, langs)) in ids.iter().enumerate() {
      entry(&mut out, sub + 16 + 8 * j, id, lang_dir as u32 | SUBDIR);
      put(&mut out, lang_dir + 14, &(langs.len() as u16).to_le_bytes());
      for (k, (&lang, data)) in langs.iter().enumerate() {
        let at = entries + 16 * leaf;
        entry(&mut out, lang_dir + 16 + 8 * k, lang, at as u32);
        let to = rva + out.len() as u32;
        put(&mut out, at, &to.to_le_bytes());
        put(&mut out, at + 4, &(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out.resize(align(out.len() as u64, 8) as usize, 0);
        leaf += 1;
      }

      lang_dir += dir(langs.len());
    }

    sub += dir(ids.len());
  }

  out
}

/// A directory entry at `at`: the id and where it points.
fn entry(out: &mut [u8], at: usize, id: u16, to: u32) {
  put(out, at, &(id as u32).to_le_bytes());
  put(out, at + 4, &to.to_le_bytes());
}

/// The PE checksum of `data`, whose own field is zero: the 16-bit words' sum, its carries folded
/// in, plus the file's length.
fn checksum(data: &[u8]) -> u32 {
  let mut sum = 0u32;
  for w in data.chunks(2) {
    sum += u16::from_le_bytes([w[0], w.get(1).copied().unwrap_or(0)]) as u32;
    sum = (sum & 0xFFFF) + (sum >> 16);
  }

  ((sum + (sum >> 16)) & 0xFFFF).wrapping_add(data.len() as u32)
}

fn arch(machine: u16) -> String {
  match machine {
    AMD64 => "x64".into(),
    ARM64 => "arm64".into(),
    0x14C => "x86".into(),
    _ => format!("machine {machine:#06x}"),
  }
}

fn align(n: u64, to: u64) -> u64 {
  n.div_ceil(to) * to
}

fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
  out[at..at + bytes.len()].copy_from_slice(bytes);
}

fn short() -> io::Error {
  bad("the exe ends inside its headers")
}

fn u16_at(data: &[u8], at: usize) -> io::Result<u16> {
  let b = data.get(at..at + 2).ok_or_else(short)?;
  Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
  let b = data.get(at..at + 4).ok_or_else(short)?;
  Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
pub mod tests {
  use super::*;

  /// A PE32+ exe for `machine` as a linker lays it out: 0x400 bytes, the headers in the first
  /// 0x200, one `.text` section at 0x1000 in the second; its last 5 bytes are `ret` 4 times
  /// after a `nop`, so it is not all zeros.
  pub fn exe(machine: u16) -> Vec<u8> {
    let mut pe = vec![0; 0x400];
    put(&mut pe, 0, b"MZ");
    put(&mut pe, 0x3C, &0x40u32.to_le_bytes());
    put(&mut pe, 0x40, b"PE\0\0");
    put(&mut pe, 0x44, &machine.to_le_bytes());
    put(&mut pe, 0x46, &1u16.to_le_bytes());
    put(&mut pe, 0x54, &240u16.to_le_bytes());
    put(&mut pe, 0x56, &0x22u16.to_le_bytes());
    let opt = [(0, 0x20Bu32, 2), (16, 0x1000, 4), (32, 0x1000, 4), (36, 0x200, 4)];
    let opt = opt.into_iter().chain([(56, 0x2000, 4), (60, 0x200, 4), (108, 16, 4)]);
    for (at, x, n) in opt {
      put(&mut pe, 0x58 + at, &x.to_le_bytes()[..n]);
    }

    put(&mut pe, 0x148, b".text\0\0\0");
    for (i, x) in [16u32, 0x1000, 0x200, 0x200, 0, 0, 0, 0x6000_0020].iter().enumerate() {
      put(&mut pe, 0x150 + 4 * i, &x.to_le_bytes());
    }

    put(&mut pe, 0x200, &[0x90, 0xC3, 0xC3, 0xC3, 0xC3]);
    pe
  }

  /// Checks that the headers of `data` agree with each other: the sections aligned, in order and
  /// apart in memory and in the file, SizeOfImage their end, the resource directory in one, and
  /// the checksum the file's.
  pub fn consistent(data: &[u8]) {
    let pe = Pe::read(data).unwrap();
    let (fa, sa) = (pe.file_align, pe.section_align);
    let mut end = (align(pe.headers_size as u64, fa as u64) as u32, pe.headers_size);
    for s in &pe.sections {
      assert_eq!((s.va % sa, s.raw_at % fa, s.raw_size % fa), (0, 0, 0));
      assert!(s.va >= align(end.1 as u64, sa as u64) as u32, "{:#x} overlaps", s.va);
      assert!(s.raw_at >= end.0 && (s.raw_at + s.raw_size) as usize <= data.len());
      end = (s.raw_at + s.raw_size, s.va + s.vsize);
    }

    assert_eq!(pe.image_size as u64, align(end.1 as u64, sa as u64));
    let dir = [16, 20].map(|at| u32_at(data, pe.opt + DIRS + at).unwrap());
    pe.slice(data, dir[0], dir[1]).unwrap();
    let mut zeroed = data.to_vec();
    put(&mut zeroed, pe.opt + 64, &[0; 4]);
    assert_eq!(u32_at(data, pe.opt + 64).unwrap(), checksum(&zeroed));
  }

  /// A tree of one resource per `(type, id, data)`, each in [LANG].
  fn tree<const N: usize>(items: [(u16, u16, &[u8]); N]) -> Tree {
    let mut tree = Tree::new();
    for (kind, id, data) in items {
      tree.entry(kind).or_default().entry(id).or_default().insert(LANG, data.to_vec());
    }

    tree
  }

  #[test]
  fn section_added() {
    let ours = tree([(RT_MANIFEST, 1, b"<m/>"), (RT_ICON, 1, b"a"), (RT_ICON, 2, b"bc")]);
    let out = with_resources(&exe(AMD64), AMD64, ours.clone()).unwrap();
    consistent(&out);
    assert_eq!(resources(&out).unwrap(), ours);

    let pe = Pe::read(&out).unwrap();
    assert_eq!(pe.sections.len(), 2);
    assert_eq!(&out[0x170..0x178], b".rsrc\0\0\0");
    let rsrc = &pe.sections[1];
    assert_eq!(
      (rsrc.va, rsrc.raw_at, rsrc.raw_size, pe.image_size),
      (0x2000, 0x400, 0x200, 0x3000)
    );
    assert_eq!(u32_at(&out, 0x170 + 36).unwrap(), RSRC);
    assert_eq!(u32_at(&out, pe.opt + DIRS + 16).unwrap(), 0x2000);
    assert_eq!(u32_at(&out, pe.opt + DIRS + 20).unwrap(), rsrc.vsize);
    assert_eq!(u32_at(&out, pe.opt + 8).unwrap(), 0x200);
    assert_eq!(out.len(), 0x600);
    assert_eq!(out[0x200..0x400], exe(AMD64)[0x200..0x400]);
  }

  #[test]
  fn directory_layout() {
    // The root, a directory per type and per id, sorted by number, then the data at 8 bytes.
    let data =
      rsrc(&tree([(RT_MANIFEST, 1, b"<m/>"), (RT_ICON, 2, b"bc"), (RT_ICON, 1, b"a")]), 0x5000);
    let ids = |at: usize| entries(&data, at, true, &mut 100).unwrap();
    assert_eq!(ids(0), [(RT_ICON, 32), (RT_MANIFEST, 64)]);
    assert_eq!(ids(32), [(1, 88), (2, 112)]);
    assert_eq!(ids(64), [(1, 136)]);
    assert_eq!(entries(&data, 88, false, &mut 100).unwrap(), [(LANG, 160)]);
    assert_eq!(entries(&data, 136, false, &mut 100).unwrap(), [(LANG, 192)]);
    let (rva, size) = (u32_at(&data, 192).unwrap(), u32_at(&data, 196).unwrap());
    assert_eq!((rva, size), (0x5000 + 224, 4));
    assert_eq!(&data[208..209], b"a");
    assert_eq!(&data[224..], b"<m/>\0\0\0\0");
  }

  #[test]
  fn others_kept() {
    // A first edit's resources stand for the linker's: the types written again are replaced.
    let first = tree([(6, 1, b"strings"), (RT_MANIFEST, 1, b"old"), (RT_MANIFEST, 2, b"old")]);
    let exe = with_resources(&exe(ARM64), ARM64, first).unwrap();
    let out = with_resources(&exe, ARM64, tree([(RT_MANIFEST, 1, b"new")])).unwrap();
    consistent(&out);
    assert_eq!(resources(&out).unwrap(), tree([(6, 1, b"strings"), (RT_MANIFEST, 1, b"new")]));
    assert_eq!(Pe::read(&out).unwrap().sections.len(), 3);
  }

  #[test]
  fn refused() {
    let ok = tree([(RT_MANIFEST, 1, b"<m/>")]);
    let err =
      |data: &[u8], machine| with_resources(data, machine, ok.clone()).unwrap_err().to_string();
    assert_eq!(err(&exe(AMD64), ARM64), "the exe is for x64, not arm64");
    assert_eq!(err(b"ELF", AMD64), "not an exe: no MZ header");

    let mut pe32 = exe(AMD64);
    put(&mut pe32, 0x58, &0x10Bu16.to_le_bytes());
    assert_eq!(err(&pe32, AMD64), "not a 64-bit exe (PE32+)");

    let mut signed = exe(AMD64);
    put(&mut signed, 0x58 + DIRS + 36, &8u32.to_le_bytes());
    assert_eq!(err(&signed, AMD64), "the exe is signed: its resources go in before its signature");

    let mut full = exe(AMD64);
    put(&mut full, 0x58 + 60, &0x170u32.to_le_bytes());
    assert_eq!(err(&full, AMD64), "its headers have no room for one more section header");
    let mut taken = exe(AMD64);
    put(&mut taken, 0x170, b".x");
    assert_eq!(err(&taken, AMD64), "its headers have no room for one more section header");

    let mut named = with_resources(&exe(AMD64), AMD64, ok.clone()).unwrap();
    put(&mut named, 0x400 + 12, &1u16.to_le_bytes());
    assert_eq!(err(&named, AMD64), "a resource named by a string, which the packer can't keep");

    let mut small = exe(AMD64);
    put(&mut small, 0x58 + 36, &0x100u32.to_le_bytes());
    let want = "its file alignment 0x100 or section alignment 0x1000 is not an exe's: a power of \
                two, 512 to 64K and at least 4K";
    assert_eq!(err(&small, AMD64), want);
    let mut big = exe(AMD64);
    put(&mut big, 0x58 + 36, &0x20000u32.to_le_bytes());
    assert!(err(&big, AMD64).starts_with("its file alignment 0x20000 or"));
    let mut driver = exe(AMD64);
    put(&mut driver, 0x58 + 32, &0x200u32.to_le_bytes());
    assert!(err(&driver, AMD64).contains("section alignment 0x200 is not"));

    // Cut anywhere, it is an error, never a panic.
    let whole = with_resources(&exe(AMD64), AMD64, ok.clone()).unwrap();
    for n in 0..0x420 {
      assert!(with_resources(&whole[..n], AMD64, ok.clone()).is_err(), "{n}");
    }
  }

  #[test]
  fn named_replaced() {
    // An icon group named by a string, as embed-resource's `APP ICON`: fine when replaced.
    let first = tree([(6, 1, b"strings"), (RT_GROUP_ICON, 1, b"group")]);
    let mut exe = with_resources(&exe(AMD64), AMD64, first).unwrap();
    // The root holds 2 types, so the group's directory follows type 6's, at 32 + 24.
    assert_eq!(entries(&exe[0x400..], 0, true, &mut 100).unwrap()[1], (RT_GROUP_ICON, 56));
    put(&mut exe, 0x400 + 56 + 12, &1u16.to_le_bytes());
    put(&mut exe, 0x400 + 56 + 14, &0u16.to_le_bytes());
    put(&mut exe, 0x400 + 56 + 16, &(SUBDIR | 0x100).to_le_bytes());

    let out = with_resources(&exe, AMD64, tree([(RT_GROUP_ICON, 1, b"ours")])).unwrap();
    assert_eq!(resources(&out).unwrap(), tree([(6, 1, b"strings"), (RT_GROUP_ICON, 1, b"ours")]));
    let e = with_resources(&exe, AMD64, tree([(RT_MANIFEST, 1, b"<m/>")])).unwrap_err();
    assert_eq!(e.to_string(), "a resource named by a string, which the packer can't keep");
  }

  #[test]
  fn loops_end() {
    let start = std::time::Instant::now();
    let read = |_, _| Ok(&b"x"[..]);
    let looped = "its resource directory points into itself";

    // The root points at itself as each type's directory, so each id's too.
    let mut dir = vec![0; 16 + 8 * 1000];
    put(&mut dir, 14, &1000u16.to_le_bytes());
    for i in 0..1000 {
      entry(&mut dir, 16 + 8 * i, i as u16 + 1, SUBDIR);
    }

    let e = walk(&dir, |_| false, 1 << 20, read).unwrap_err().to_string();
    assert!(e == looped || e == "its resource directory is malformed", "{e}");

    // Three levels of 1000 entries, each pointing at the next level's one directory: a billion
    // leaves, all at one data entry.
    let level = 16 + 8 * 1000;
    let mut dir = vec![0; 3 * level + 16];
    for (n, sub) in [(0, SUBDIR), (1, SUBDIR), (2, 0)] {
      put(&mut dir, n * level + 14, &1000u16.to_le_bytes());
      for i in 0..1000 {
        let to = if sub == 0 { 3 * level } else { (n + 1) * level } as u32;
        entry(&mut dir, n * level + 16 + 8 * i, i as u16, to | sub);
      }
    }

    put(&mut dir, 3 * level + 4, &1u32.to_le_bytes());
    assert_eq!(walk(&dir, |_| false, 1 << 20, read).unwrap_err().to_string(), looped);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
  }

  #[test]
  fn sum() {
    assert_eq!(checksum(&[1, 0, 2, 0]), 3 + 4);
    assert_eq!(checksum(&[0xFF, 0xFF, 2, 0, 7]), 2 + 7 + 5);
    assert_eq!(checksum(&[]), 0);
  }
}
