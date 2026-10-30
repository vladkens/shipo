// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The `.deb` format of deb(5): an `ar` archive of `debian-binary` (`2.0`), `control.tar.gz`
//! holding the control file and `data.tar.gz` holding the files, each a tar archive under `./`.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::GzEncoder;
use tar::{Builder, EntryType, Header};

use crate::tree::{Node, Tree};

/// The `.deb` of the control file `control` and the files `data`, all of the time `mtime`.
pub fn build(control: &str, data: &Tree, mtime: u64) -> io::Result<Vec<u8>> {
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
}
