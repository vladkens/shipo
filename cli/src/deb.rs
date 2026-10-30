// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The `.deb` format of deb(5): an `ar` archive of `debian-binary` (`2.0`), `control.tar.gz`
//! holding the control file and `data.tar.gz` holding the files, each tar of `./` paths.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::tar;
use crate::tree::Tree;

/// The `.deb` of the control file `control` and the files `data`, all of the time `mtime`.
pub fn build(control: &str, data: &Tree, mtime: u64) -> io::Result<Vec<u8>> {
  let mut meta = Tree::default();
  meta.file("control", 0o644, control);

  let mut out = b"!<arch>\n".to_vec();
  member(&mut out, "debian-binary", b"2.0\n", mtime);
  member(&mut out, "control.tar.gz", &gzip(&tar::write(&meta, mtime)?)?, mtime);
  member(&mut out, "data.tar.gz", &gzip(&tar::write(data, mtime)?)?, mtime);
  Ok(out)
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

#[cfg(test)]
mod tests {
  use std::io::Read;

  use flate2::read::GzDecoder;

  use super::*;

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
    let deb = build("Package: hello\n", &tree, 1_700_000_000).unwrap();

    let members = members(&deb);
    let heads: Vec<_> = members.iter().map(|(head, _)| head[..48].to_string()).collect();
    let want = ["debian-binary", "control.tar.gz", "data.tar.gz"]
      .map(|name| format!("{name:<16}1700000000  0     0     100644  "));
    assert_eq!(heads, want);
    assert_eq!(members[0].1, b"2.0\n");

    let mut meta = Tree::default();
    meta.file("control", 0o644, "Package: hello\n");
    assert_eq!(gunzip(&members[1].1), tar::write(&meta, 1_700_000_000).unwrap());
    assert_eq!(gunzip(&members[2].1), tar::write(&tree, 1_700_000_000).unwrap());
    let files = tar::files(&members[2].1).unwrap();
    assert_eq!(files[0], ("usr/bin/hello".into(), b"exe".to_vec()));
  }
}
