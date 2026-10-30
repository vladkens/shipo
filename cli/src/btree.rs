// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! An HFS+ B-tree file of sorted records (TN1150, "B-Trees"): the header node, the leaves and the
//! index nodes above them, all written at once and never changed, so no node is left free.

use std::io;

use crate::bad;

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
pub const BIG_KEYS: u32 = 2;
pub const VARIABLE_INDEX_KEYS: u32 = 4;

/// What sets one tree apart from another.
pub struct Tree {
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
pub fn file(tree: &Tree, records: &[(Vec<u8>, Vec<u8>)]) -> io::Result<Vec<u8>> {
  // A level's nodes, each its records; the leaves first. A level of one node is the root.
  let mut levels: Vec<Vec<Vec<Vec<u8>>>> = vec![];
  let leaves = records.iter().map(|(key, data)| [key.as_slice(), data].concat());
  let mut level = pack(leaves.collect(), 0);
  while !level.is_empty() {
    let root = level.len() == 1;
    // An index record is the first key of a node below, then its number, added below.
    let keys = level.iter().map(|node| key(&node[0]).to_vec()).collect();
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
fn key(record: &[u8]) -> &[u8] {
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
mod tests {
  use super::*;

  fn word(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
  }

  fn half(data: &[u8], at: usize) -> usize {
    u16::from_be_bytes([data[at], data[at + 1]]) as usize
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
    let file = file(&TREE, &[]).unwrap();
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
    let file = file(&TREE, &all).unwrap();
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
