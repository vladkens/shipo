// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The tar format: the regular files read out of a gzipped tarball, as a fetched tool comes, and
//! a ustar archive written of a [Tree], as a `.deb` holds one. Headers are POSIX ustar
//! (pubs.opengroup.org, `pax`); the reader also takes GNU's long names and pax's `path`.

use std::io::{self, Read};

use flate2::read::MultiGzDecoder;

use crate::bad;
use crate::tree::{Node, Tree};

const BLOCK: usize = 512;

/// The regular files of the gzipped tarball `gz`, each by its path without a leading `./`; a path
/// that is absolute or holds `..` is an error, whatever its type.
pub fn files(gz: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
  let mut data = vec![];
  MultiGzDecoder::new(gz).read_to_end(&mut data)?;

  let (mut out, mut at, mut long) = (vec![], 0, None::<String>);
  loop {
    // Past the last entry: the end's zero blocks, or nothing; anything else is a cut header.
    let Some(head) = data.get(at..at + BLOCK) else {
      match at < data.len() {
        true => return Err(bad("the tarball is cut short")),
        false => break,
      }
    };

    if head.iter().all(|&b| b == 0) {
      break;
    }

    if octal(&head[148..156])? != checksum(head) {
      return Err(bad(format!("a tar header at {at} has a wrong checksum")));
    }

    let size = octal(&head[124..136])? as usize;
    let body =
      data.get(at + BLOCK..at + BLOCK + size).ok_or_else(|| bad("the tarball is cut short"))?;
    at += BLOCK + size.next_multiple_of(BLOCK);
    match head[156] {
      b'L' => long = Some(text(body)),
      b'x' => long = pax_path(body).or(long),
      b'g' => {}
      kind => {
        let name = long.take().unwrap_or_else(|| name(head));
        let name = checked(&name)?;
        // `7` is a contiguous file, read as a regular one.
        if matches!(kind, b'0' | b'7' | 0) {
          out.push((name, body.to_vec()));
        }
      }
    }
  }

  Ok(out)
}

/// The sum of the bytes of the header `head`, its checksum field taken as spaces.
fn checksum(head: &[u8]) -> u64 {
  let field = |i: usize| (148..156).contains(&i);
  head.iter().enumerate().map(|(i, &b)| if field(i) { 32 } else { b as u64 }).sum()
}

/// The name in the ustar header `head`: its prefix, if any, a `/` and its name.
fn name(head: &[u8]) -> String {
  let name = text(&head[..100]);
  match &head[257..263] == b"ustar\0" && head[345] != 0 {
    true => format!("{}/{name}", text(&head[345..500])),
    false => name,
  }
}

/// `name` without its leading `./`; an error when it is absolute or holds `..`.
fn checked(name: &str) -> io::Result<String> {
  let mut rest = name;
  while let Some(r) = rest.strip_prefix("./") {
    rest = r;
  }

  match rest.starts_with('/') || rest.split('/').any(|part| part == "..") {
    true => Err(bad(format!("the tarball holds {name:?}, outside its folder"))),
    false => Ok(rest.to_string()),
  }
}

/// The `path` record of the pax extended header `body`, records being `<len> <key>=<value>\n`.
fn pax_path(body: &[u8]) -> Option<String> {
  let mut rest = body;
  while !rest.is_empty() {
    let space = rest.iter().position(|&b| b == b' ')?;
    let len: usize = std::str::from_utf8(&rest[..space]).ok()?.parse().ok()?;
    let record = rest.get(space + 1..len)?.strip_suffix(b"\n")?;
    if let Some(path) = record.strip_prefix(b"path=") {
      return Some(String::from_utf8_lossy(path).into_owned());
    }

    rest = &rest[len..];
  }

  None
}

/// The text of the field `field`, up to its first NUL.
fn text(field: &[u8]) -> String {
  let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
  String::from_utf8_lossy(&field[..end]).into_owned()
}

/// The number in the octal field `field`, padded by spaces or NULs.
fn octal(field: &[u8]) -> io::Result<u64> {
  let digits = text(field);
  let digits = digits.trim_matches(' ');
  match digits.is_empty() {
    true => Ok(0),
    false => u64::from_str_radix(digits, 8)
      .map_err(|_| bad(format!("a tar field {digits:?} is not octal"))),
  }
}

// MARK: Writing

/// The ustar archive of `tree` under `./`, `./` itself first, each entry owned by root and of the
/// time `mtime`, ended by two zero blocks. A path or a link ustar can't hold is an error.
pub fn write(tree: &Tree, mtime: u64) -> io::Result<Vec<u8>> {
  let mut out = header("./", 0o755, b'5', 0, "", mtime)?;
  for (path, mode, node) in tree.iter() {
    match node {
      Node::Dir => out.extend(header(&format!("./{path}/"), mode, b'5', 0, "", mtime)?),
      Node::Link(to) => out.extend(header(&format!("./{path}"), mode, b'2', 0, to, mtime)?),
      Node::File(data) => {
        out.extend(header(&format!("./{path}"), mode, b'0', data.len() as u64, "", mtime)?);
        out.extend(data);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
      }
    }
  }

  out.resize(out.len() + 2 * BLOCK, 0);
  Ok(out)
}

/// The ustar header of `path`, of the Unix `mode`, the type `kind`, `size` bytes, the link target
/// `link` and the time `mtime`, owned by root.
fn header(
  path: &str,
  mode: u32,
  kind: u8,
  size: u64,
  link: &str,
  mtime: u64,
) -> io::Result<Vec<u8>> {
  let (prefix, name) = split(path)?;
  if link.len() > 100 {
    return Err(bad(format!("the link {path} -> {link} is too long for a tar header")));
  }

  // 11 octal digits each.
  if size >= 1 << 33 || mtime >= 1 << 33 {
    return Err(bad(format!("{path} is too large for a tar header")));
  }

  let mut head = vec![0u8; BLOCK];
  let mut put = |at: usize, field: &[u8]| head[at..at + field.len()].copy_from_slice(field);
  put(0, name.as_bytes());
  put(100, format!("{:07o}\0", mode & 0o7777).as_bytes());
  put(108, b"0000000\0");
  put(116, b"0000000\0");
  put(124, format!("{size:011o}\0").as_bytes());
  put(136, format!("{mtime:011o}\0").as_bytes());
  put(156, &[kind]);
  put(157, link.as_bytes());
  put(257, b"ustar\0");
  put(263, b"00");
  put(265, b"root");
  put(297, b"root");
  put(329, b"0000000\0");
  put(337, b"0000000\0");
  put(345, prefix.as_bytes());
  let sum = checksum(&head);
  head[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
  Ok(head)
}

/// `path` as a ustar header holds it: a prefix of up to 155 bytes, cut at a `/`, and a name of up
/// to 100; an error when no cut fits.
fn split(path: &str) -> io::Result<(&str, &str)> {
  if path.len() <= 100 {
    return Ok(("", path));
  }

  // The longest prefix leaves the shortest name; a folder's own trailing `/` is no cut. Bytes, as
  // byte 156 may fall inside a character.
  match path.as_bytes()[..156.min(path.len() - 1)].iter().rposition(|&b| b == b'/') {
    Some(i) if path.len() - i - 1 <= 100 => Ok((&path[..i], &path[i + 1..])),
    _ => Err(bad(format!("{path} is too long for a tar header"))),
  }
}

#[cfg(test)]
mod tests {
  use std::io::Write;

  use flate2::Compression;
  use flate2::write::GzEncoder;

  use super::*;

  /// A ustar header and the padded body of `name` of the type `kind`, holding `body`.
  fn entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut head = [0u8; BLOCK];
    head[..name.len()].copy_from_slice(name.as_bytes());
    head[100..107].copy_from_slice(b"0000755");
    head[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
    head[156] = kind;
    head[257..263].copy_from_slice(b"ustar\0");
    let sum = checksum(&head);
    head[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());

    let mut out = head.to_vec();
    out.extend(body);
    out.resize(out.len().next_multiple_of(BLOCK), 0);
    out
  }

  fn gz(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut enc = GzEncoder::new(vec![], Compression::default());
    for e in entries {
      enc.write_all(e).unwrap();
    }

    enc.write_all(&[0; 2 * BLOCK]).unwrap();
    enc.finish().unwrap()
  }

  #[test]
  fn read() {
    let long = format!("{}/tool", "d".repeat(120));
    let tarball = gz(&[
      entry("./d/", b'5', b""),
      entry("./d/tool", b'0', b"tool"),
      entry("d/link", b'2', b""),
      entry("././@LongLink", b'L', long.as_bytes()),
      entry("cut", b'0', b"long"),
      entry("PaxHeader", b'x', b"17 path=pax/tool\n"),
      entry("cut", b'0', b"pax"),
      entry("d/empty", 0, b""),
      entry("d/contiguous", b'7', b"c"),
    ]);

    let want = [
      ("d/tool", "tool"),
      (long.as_str(), "long"),
      ("pax/tool", "pax"),
      ("d/empty", ""),
      ("d/contiguous", "c"),
    ];
    let want: Vec<_> = want.map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).into();
    assert_eq!(files(&tarball).unwrap(), want);

    for name in ["/etc/passwd", "d/../../x", "..", "./../x"] {
      let e = files(&gz(&[entry(name, b'0', b"x")])).unwrap_err().to_string();
      assert_eq!(e, format!("the tarball holds {name:?}, outside its folder"));
    }

    // A link out of the folder is refused too, though only regular files are taken.
    assert!(files(&gz(&[entry("../link", b'2', b"")])).is_err());

    let mut bad = entry("x", b'0', b"x");
    bad[0] = b'y';
    let e = files(&gz(&[bad])).unwrap_err().to_string();
    assert_eq!(e, "a tar header at 0 has a wrong checksum");

    let mut cut = gz(&[entry("x", b'0', &[1; 600])]);
    cut = {
      let mut data = vec![];
      MultiGzDecoder::new(&cut[..]).read_to_end(&mut data).unwrap();
      let mut enc = GzEncoder::new(vec![], Compression::default());
      enc.write_all(&data[..BLOCK + 100]).unwrap();
      enc.finish().unwrap()
    };
    assert_eq!(files(&cut).unwrap_err().to_string(), "the tarball is cut short");

    // Cut inside the second header, before the end's zero blocks.
    let mut head = entry("x", b'0', b"x");
    head.extend(&entry("y", b'0', b"y")[..100]);
    let mut enc = GzEncoder::new(vec![], Compression::default());
    enc.write_all(&head).unwrap();
    let e = files(&enc.finish().unwrap()).unwrap_err().to_string();
    assert_eq!(e, "the tarball is cut short");
    assert!(files(b"not gzip").is_err());
  }

  #[test]
  fn written() {
    let mut tree = Tree::default();
    let deep = format!("{}/{}/{}", "a".repeat(90), "c".repeat(60), "b".repeat(90));
    tree.file("usr/bin/hello", 0o755, "exe");
    tree.link("usr/bin/hi", "hello");
    tree.file(&deep, 0o644, vec![7; 600]);
    let data = write(&tree, 1_700_000_000).unwrap();
    assert_eq!(data.len() % BLOCK, 0);
    assert!(data[data.len() - 2 * BLOCK..].iter().all(|&b| b == 0));

    // Each header, its fields and its checksum, then the body padded to a block.
    let mut heads = vec![];
    let mut at = 0;
    while data[at..at + BLOCK].iter().any(|&b| b != 0) {
      let head = &data[at..at + BLOCK];
      assert_eq!(octal(&head[148..156]).unwrap(), checksum(head));
      assert_eq!(
        (&head[257..263], &head[263..265], text(&head[265..297]), text(&head[297..329])),
        (&b"ustar\0"[..], &b"00"[..], "root".into(), "root".into())
      );
      assert_eq!((octal(&head[108..116]).unwrap(), octal(&head[116..124]).unwrap()), (0, 0));
      assert_eq!(octal(&head[136..148]).unwrap(), 1_700_000_000);
      let mode = octal(&head[100..108]).unwrap();
      heads.push(format!(
        "{} {} {mode:o} {}",
        name(head),
        head[156] as char,
        text(&head[157..257])
      ));
      at += BLOCK + (octal(&head[124..136]).unwrap() as usize).next_multiple_of(BLOCK);
    }

    let want = [
      "./ 5 755 ".to_string(),
      format!("./{}/ 5 755 ", "a".repeat(90)),
      format!("./{}/{}/ 5 755 ", "a".repeat(90), "c".repeat(60)),
      format!("./{deep} 0 644 "),
      "./usr/ 5 755 ".into(),
      "./usr/bin/ 5 755 ".into(),
      "./usr/bin/hello 0 755 ".into(),
      "./usr/bin/hi 2 777 hello".into(),
    ];
    assert_eq!(heads, want);
    assert_eq!(at, data.len() - 2 * BLOCK);

    let mut enc = GzEncoder::new(vec![], Compression::default());
    enc.write_all(&data).unwrap();
    let read = files(&enc.finish().unwrap()).unwrap();
    assert_eq!(read, [(deep.clone(), vec![7; 600]), ("usr/bin/hello".into(), b"exe".to_vec())]);

    // A name past 100 bytes at every cut that leaves a prefix of up to 155, a folder's included.
    let cases = [
      (format!("{}/b", "a".repeat(160)), format!("./{}/", "a".repeat(160))),
      (format!("a/{}", "b".repeat(101)), format!("./a/{}", "b".repeat(101))),
    ];

    for (path, at) in cases {
      let mut tree = Tree::default();
      tree.file(&path, 0o644, "");
      let e = write(&tree, 0).unwrap_err().to_string();
      assert_eq!(e, format!("{at} is too long for a tar header"));
    }

    let mut tree = Tree::default();
    tree.link("l", &"x".repeat(101));
    assert!(write(&tree, 0).unwrap_err().to_string().ends_with("is too long for a tar header"));
  }
}
