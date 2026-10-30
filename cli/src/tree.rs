// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A package's files in memory: folders, files and links by path, with their modes, which the
//! `.deb` and squashfs writers put in their archives as they are, owned by root.

use std::collections::BTreeMap;

/// What a path holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
  Dir,
  File(Vec<u8>),
  Link(String),
}

/// The entries under a root folder (0755), each by its path relative to it, `a/b`, with its Unix
/// mode (a link's is 0777). A path's folders are entries too, in byte order before it.
#[derive(Debug, Default)]
pub struct Tree(BTreeMap<String, (u32, Node)>);

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
mod tests {
  use super::*;

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
