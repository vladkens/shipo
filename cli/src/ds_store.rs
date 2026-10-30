// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The DMG's `.DS_Store`: the window Finder opens on the volume, the app and the Applications link
//! side by side, in the format Wim Lewis's DSStoreFormat.pod documents.

use std::io;

use crate::bad;
use crate::plist::Plist;

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
pub fn ds_store(app: &str) -> io::Result<Vec<u8>> {
  let window = Plist::dict([
    ("WindowBounds", "{{200, 120}, {640, 400}}".into()),
    ("SidebarWidth", Plist::Int(180)),
    ("ContainerShowSidebar", false.into()),
    ("PreviewPaneVisibility", false.into()),
    ("ShowToolbar", false.into()),
    ("ShowSidebar", false.into()),
    ("ShowStatusBar", false.into()),
    ("ShowPathbar", false.into()),
    ("ShowTabView", false.into()),
  ]);

  let view = Plist::dict([
    ("viewOptionsVersion", Plist::Int(1)),
    ("backgroundType", Plist::Int(0)),
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
    (".", b"bwsp", b"blob", blob(&window.binary())),
    (".", b"icvl", b"type", b"icnv".to_vec()),
    (".", b"icvp", b"blob", blob(&view.binary())),
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

fn push(out: &mut Vec<u8>, words: &[u32]) {
  words.iter().for_each(|w| out.extend_from_slice(&w.to_be_bytes()));
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  /// A big-endian word of `data` at `at`.
  pub fn word(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
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
  fn layout() {
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
      let hex: String = data[4..].iter().map(|b| format!("{b:02x}")).collect();
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
