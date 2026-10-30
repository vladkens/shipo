// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A UDIF disk image compressed by zlib (UDZO, as `hdiutil` makes it) of a disk's bytes: the
//! chunks, the `blkx` table in the XML property list, and the `koly` trailer.
//!
//! The file: the data fork (each 1 MiB chunk of the disk, zlib-compressed at level 6, raw where it
//! doesn't shrink it, nothing for one all zeros), the property list, whose `resource-fork` holds
//! the one `blkx` table of the whole disk, and the 512-byte trailer. Each checksum is a CRC-32
//! (zlib's): the table's of the disk's bytes, the trailer's data fork one of the data fork, and its
//! master one of the tables' checksums, as `hdiutil` writes them.

use std::io::{self, Write};

use flate2::Crc;
use flate2::write::ZlibEncoder;

/// The sector, and a chunk: 1 MiB, of 2048 sectors.
const SECTOR: usize = 512;
const CHUNK: usize = 2048;

/// The chunk types of a `blkx` table.
const ZERO: u32 = 0;
const RAW: u32 = 1;
const ZLIB: u32 = 0x8000_0005;
const END: u32 = 0xFFFF_FFFF;

/// The checksum type of a CRC-32.
const CRC32: u32 = 2;

/// Writes the UDZO image of `disk`, whose length is whole sectors, to `out`, a chunk at a time,
/// the partition named `name` in its table, as `Apple_HFS` for an HFS+ volume.
pub fn udzo(disk: &[u8], name: &str, out: &mut impl Write) -> io::Result<()> {
  let sectors = (disk.len() / SECTOR) as u64;
  let (mut disk_crc, mut data_crc) = (Crc::new(), Crc::new());
  // The segment's id, of the data fork.
  let mut id = hmac_sha256::Hash::new();
  let mut packed = Vec::with_capacity(CHUNK * SECTOR);
  let mut chunks = vec![];
  let mut offset = 0u64;
  for (i, chunk) in disk.chunks(CHUNK * SECTOR).enumerate() {
    let (start, count) = ((i * CHUNK) as u64, (chunk.len() / SECTOR) as u64);
    disk_crc.update(chunk);
    let (kind, data) = match chunk.iter().all(|&b| b == 0) {
      true => (ZERO, &[][..]),
      false => {
        packed.clear();
        let mut z = ZlibEncoder::new(&mut packed, flate2::Compression::default());
        z.write_all(chunk)?;
        z.finish()?;
        match packed.len() < chunk.len() {
          true => (ZLIB, &packed[..]),
          false => (RAW, chunk),
        }
      }
    };

    out.write_all(data)?;
    data_crc.update(data);
    id.update(data);
    chunks.push((kind, start, count, offset, data.len() as u64));
    offset += data.len() as u64;
  }

  chunks.push((END, sectors, 0, offset, 0));
  let mut blkx = vec![];
  blkx.extend_from_slice(b"mish");
  push(&mut blkx, &[1]);
  push64(&mut blkx, &[0, sectors, 0]);
  // The buffer a chunk needs, in sectors, then the partition's number: one, 0.
  push(&mut blkx, &[CHUNK as u32 + 8, 0]);
  blkx.extend_from_slice(&[0; 24]);
  blkx.extend_from_slice(&checksum(disk_crc.sum()));
  push(&mut blkx, &[chunks.len() as u32]);
  for (kind, start, count, offset, length) in &chunks {
    push(&mut blkx, &[*kind, 0]);
    push64(&mut blkx, &[*start, *count, *offset, *length]);
  }

  let plist = plist(&blkx, name);
  out.write_all(plist.as_bytes())?;

  let mut koly = vec![];
  koly.extend_from_slice(b"koly");
  // Version 4, the trailer's size, flattened.
  push(&mut koly, &[4, 512, 1]);
  // The data fork's running and own offset and its length; no resource fork.
  push64(&mut koly, &[0, 0, offset, 0, 0]);
  // Segment 1 of 1, and its id.
  push(&mut koly, &[1, 1]);
  koly.extend_from_slice(&id.finalize()[..16]);
  koly.extend_from_slice(&checksum(data_crc.sum()));
  push64(&mut koly, &[offset, plist.len() as u64]);
  koly.extend_from_slice(&[0; 120]);
  koly.extend_from_slice(&checksum(crc(&disk_crc.sum().to_be_bytes())));
  // A device image, its sectors.
  push(&mut koly, &[1]);
  push64(&mut koly, &[sectors]);
  koly.resize(512, 0);
  out.write_all(&koly)
}

/// The property list of the one `blkx` table.
fn plist(blkx: &[u8], name: &str) -> String {
  let name = name.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
  let data: Vec<String> = base64(blkx)
    .as_bytes()
    .chunks(68)
    .map(|ln| format!("\t\t\t\t{}", String::from_utf8_lossy(ln)))
    .collect();
  format!(
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>resource-fork</key>
	<dict>
		<key>blkx</key>
		<array>
			<dict>
				<key>Attributes</key>
				<string>0x0050</string>
				<key>CFName</key>
				<string>{name}</string>
				<key>Data</key>
				<data>
{data}
				</data>
				<key>ID</key>
				<string>-1</string>
				<key>Name</key>
				<string>{name}</string>
			</dict>
		</array>
	</dict>
</dict>
</plist>
"#,
    data = data.join("\n")
  )
}

/// A `UDIFChecksum` of a CRC-32: its type, its 32 bits, the value in the first of 32 words.
fn checksum(crc: u32) -> Vec<u8> {
  let mut out = vec![];
  push(&mut out, &[CRC32, 32, crc]);
  out.resize(136, 0);
  out
}

fn crc(data: &[u8]) -> u32 {
  let mut crc = Crc::new();
  crc.update(data);
  crc.sum()
}

fn push(out: &mut Vec<u8>, words: &[u32]) {
  words.iter().for_each(|w| out.extend_from_slice(&w.to_be_bytes()));
}

fn push64(out: &mut Vec<u8>, words: &[u64]) {
  words.iter().for_each(|w| out.extend_from_slice(&w.to_be_bytes()));
}

/// `data` in standard base64.
fn base64(data: &[u8]) -> String {
  const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::new();
  for group in data.chunks(3) {
    let n = group.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
    for i in 0..4 {
      match i <= group.len() {
        true => out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char),
        false => out.push('='),
      }
    }
  }

  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn base64s() {
    let cases: [(&[u8], &str); 5] =
      [(b"", ""), (b"f", "Zg=="), (b"fo", "Zm8="), (b"foo", "Zm9v"), (&[0xFB, 0xFF], "+/8=")];
    for (data, want) in cases {
      assert_eq!(base64(data), want);
    }
  }
}
