// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The icon's images: a PNG read into RGBA and written back, resized and cropped, and the `.icns`
//! and `.ico` made of them, with no `sips` or `iconutil`, so on any OS.

use std::collections::BTreeMap;
use std::path::Path;
use std::{fs, io};

use png::{BitDepth, ColorType, Transformations};

use crate::bad;

/// The sizes of the `.ico` and the exe's icon: Explorer's views and the taskbar at 100–200%.
pub const ICO: [usize; 6] = [16, 24, 32, 48, 64, 256];

/// The `.icns` entries iconutil writes: 16, 32, 128, 256, 512 and each at 2x, all PNG but ic04 and
/// ic05, ARGB: macOS 27's iconutil reads a PNG in icp4 or icp5 (16, 32) as noise (2026-10-29).
pub const ICNS: [(&[u8; 4], usize); 10] = [
  (b"ic04", 16),
  (b"ic05", 32),
  (b"ic07", 128),
  (b"ic08", 256),
  (b"ic09", 512),
  (b"ic10", 1024),
  (b"ic11", 32),
  (b"ic12", 64),
  (b"ic13", 256),
  (b"ic14", 512),
];

/// An image in 8-bit RGBA, straight alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
  pub w: usize,
  pub h: usize,
  /// The pixels row by row, 4 bytes each.
  pub px: Vec<u8>,
}

impl Image {
  /// The PNG at `path`.
  pub fn read(path: &Path) -> io::Result<Self> {
    let data = fs::read(path).map_err(|e| bad(format!("{}: {e}", path.display())))?;
    Self::decode(&data).map_err(|e| bad(format!("{}: {e}", path.display())))
  }

  /// A PNG of any colour type and depth, interlaced or not: a palette and a grey expanded, the
  /// `tRNS` as alpha, 16 bits cut to their high byte.
  pub fn decode(data: &[u8]) -> io::Result<Self> {
    let mut dec = png::Decoder::new(io::Cursor::new(data));
    dec.set_transformations(Transformations::ALPHA | Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(bad)?;
    let size = reader.output_buffer_size().ok_or_else(|| bad("the PNG is too large"))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(bad)?;
    buf.truncate(info.buffer_size());

    let px = match info.color_type {
      ColorType::Rgba => buf,
      ColorType::GrayscaleAlpha => {
        buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
      }
      other => return Err(bad(format!("a PNG read as {other:?}, not RGBA"))),
    };

    Ok(Self { w: info.width as usize, h: info.height as usize, px })
  }

  /// The image as an 8-bit RGBA PNG.
  pub fn encode(&self) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let (w, h) = (u32::try_from(self.w).map_err(bad)?, u32::try_from(self.h).map_err(bad)?);
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(ColorType::Rgba);
    enc.set_depth(BitDepth::Eight);
    let mut writer = enc.write_header().map_err(bad)?;
    writer.write_image_data(&self.px).map_err(bad)?;
    writer.finish().map_err(bad)?;
    Ok(out)
  }

  /// The image at each size, `n` × `n`, each pixel the average of the source pixels it covers,
  /// weighted by their alpha: averaging straight colours would bleed the transparent black. Each
  /// size at most the image's sides; at its own size the image as it is.
  fn resized(&self, sizes: &[usize]) -> Vec<Self> {
    let (w, h) = (self.w, self.h);
    let mut out = Vec::new();
    for &n in sizes {
      if n == w && n == h {
        out.push(self.clone());
        continue;
      }

      // Box `j` covers the columns from `ceil(j·w/n)` up to the next box's, so column `x` is in box
      // `floor(x·n/w)`; rows the same.
      let col: Vec<usize> = (0..w).map(|x| x * n / w).collect();
      let row: Vec<usize> = (0..h).map(|y| y * n / h).collect();
      // Each box's colours times alpha, its alpha and its pixel count.
      let mut sums = vec![[0u64; 5]; n * n];
      for (i, p) in self.px.as_chunks::<4>().0.iter().enumerate() {
        let sum = &mut sums[row[i / w] * n + col[i % w]];
        let a = p[3] as u64;
        for c in 0..3 {
          sum[c] += p[c] as u64 * a;
        }

        sum[3] += a;
        sum[4] += 1;
      }

      // Each rounded half up, in integers.
      let px = sums.iter().flat_map(|&[r, g, b, a, count]| {
        let c = |v: u64| if a > 0 { ((2 * v + a) / (2 * a)) as u8 } else { 0 };
        [c(r), c(g), c(b), ((2 * a + count) / (2 * count)) as u8]
      });

      out.push(Self { w: n, h: n, px: px.collect() });
    }

    out
  }

  /// The image cut to the square around its visible pixels (alpha above 8), centred: macOS icons
  /// keep a transparent margin around their tile (about a tenth on each side), Windows' taskbar
  /// and Linux docks show the image as it is.
  pub fn cropped(&self) -> io::Result<Self> {
    let (w, h) = (self.w, self.h);
    let (mut cols, mut rows) = (vec![false; w], vec![false; h]);
    for (i, p) in self.px.as_chunks::<4>().0.iter().enumerate() {
      if p[3] > 8 {
        cols[i % w] = true;
        rows[i / w] = true;
      }
    }

    let first = |v: &[bool]| v.iter().position(|&x| x);
    let last = |v: &[bool]| v.iter().rposition(|&x| x);
    let (Some(x0), Some(x1), Some(y0), Some(y1)) =
      (first(&cols), last(&cols), first(&rows), last(&rows))
    else {
      return Err(bad("the icon is transparent"));
    };

    let side = (x1 - x0).max(y1 - y0) + 1;
    // The square's corner, the tile centred in it; outside the image is transparent.
    let left = (x0 + x1).div_ceil(2) as isize - (side / 2) as isize;
    let top = (y0 + y1).div_ceil(2) as isize - (side / 2) as isize;
    let (a, b) = (left.max(0) as usize, (left + side as isize).min(w as isize) as usize);
    let mut px = vec![0; side * side * 4];
    for y in top.max(0) as usize..(top + side as isize).min(h as isize) as usize {
      let at = ((y as isize - top) as usize * side + (a as isize - left) as usize) * 4;
      px[at..at + (b - a) * 4].copy_from_slice(&self.px[(y * w + a) * 4..(y * w + b) * 4]);
    }

    Ok(Self { w: side, h: side, px })
  }
}

/// The icon at each size, `crop`ped to its tile or with its margin, by size.
pub fn icons(icon: &Image, sizes: &[usize], crop: bool) -> io::Result<BTreeMap<usize, Image>> {
  let icon = if crop { &icon.cropped()? } else { icon };
  let what = if crop { "the icon's visible tile" } else { "the icon" };
  let (w, h) = (icon.w, icon.h);
  if w != h {
    return Err(bad(format!("{what} is {w}×{h}, not square")));
  }

  let mut sizes = sizes.to_vec();
  sizes.sort_unstable();
  sizes.dedup();
  if let Some(&most) = sizes.last()
    && most > w
  {
    return Err(bad(format!("{what} is {w}×{h}, smaller than {most} px")));
  }

  Ok(sizes.iter().copied().zip(icon.resized(&sizes)).collect())
}

/// The `.icns` of the icon's `images` at [ICNS]'s sizes, as iconutil writes it: ic04 and ic05
/// `ARGB`, premultiplied by alpha, each channel in literal runs of the icns RLE (a byte, the run's
/// length less one), the rest PNG.
pub fn icns(images: &BTreeMap<usize, Image>) -> io::Result<Vec<u8>> {
  let mut body = Vec::new();
  for (kind, n) in ICNS {
    let image = images.get(&n).ok_or_else(|| bad(format!("no {n} px image for the .icns")))?;
    let data = match kind {
      b"ic04" | b"ic05" => {
        let mut data = b"ARGB".to_vec();
        let px = image.px.as_chunks::<4>().0;
        for c in [3, 0, 1, 2] {
          // Rounded half up.
          let plane = px.iter().map(|p| match c {
            3 => p[3],
            _ => ((2 * p[c] as u32 * p[3] as u32 + 255) / 510) as u8,
          });

          for run in plane.collect::<Vec<_>>().chunks(128) {
            data.push((run.len() - 1) as u8);
            data.extend_from_slice(run);
          }
        }

        data
      }
      _ => image.encode()?,
    };

    body.extend_from_slice(kind);
    body.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
    body.extend_from_slice(&data);
  }

  let mut out = b"icns".to_vec();
  out.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
  out.extend_from_slice(&body);
  Ok(out)
}

/// The `.ico` of the PNGs by size (Windows Vista and later), each at most 256 px.
pub fn ico(pngs: &BTreeMap<usize, Vec<u8>>) -> io::Result<Vec<u8>> {
  let mut at = 6 + 16 * pngs.len() as u32;
  let mut out = directory(pngs, |png| {
    let end = at.to_le_bytes().to_vec();
    at += png.len() as u32;
    Ok(end)
  })?;

  pngs.values().for_each(|png| out.extend_from_slice(png));
  Ok(out)
}

/// An exe's `RT_GROUP_ICON` of the PNGs by size, each at most 256 px: the PNGs are its `RT_ICON`s
/// in order, their ids from `first_id` on.
pub fn group_icon(pngs: &BTreeMap<usize, Vec<u8>>, first_id: u16) -> io::Result<Vec<u8>> {
  let mut id = Some(first_id);
  directory(pngs, |_| {
    let end = id.ok_or_else(|| bad("the icon's RT_ICON ids pass 65535"))?;
    id = end.checked_add(1);
    Ok(end.to_le_bytes().to_vec())
  })
}

/// An icon directory of the PNGs by size, each entry ending in `end` of its PNG: in an `.ico` the
/// PNG's offset, in an `RT_GROUP_ICON` its `RT_ICON`'s id.
fn directory(
  pngs: &BTreeMap<usize, Vec<u8>>,
  mut end: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
) -> io::Result<Vec<u8>> {
  let mut out = Vec::new();
  for x in [0, 1, pngs.len() as u16] {
    out.extend_from_slice(&x.to_le_bytes());
  }

  for (&n, png) in pngs {
    if n > 256 {
      return Err(bad(format!("a {n} px icon, above the 256 px an icon directory holds")));
    }

    // Width and height, colours and a reserved byte, then planes and bits per pixel; 256 is 0.
    out.extend_from_slice(&[(n % 256) as u8, (n % 256) as u8, 0, 0]);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&end(png)?);
  }

  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A case of [Image::decode]: a PNG's colour type and depth, its row of two pixels and its
  /// `tRNS` (none if empty), and the RGBA it gives. A palette's `PLTE` is `abcdef`.
  type Case = (ColorType, BitDepth, &'static [u8], &'static [u8], &'static [u8]);

  /// The PNG of a [Case].
  fn png(&(color, depth, data, trns, _): &Case) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 2, 1);
    enc.set_color(color);
    enc.set_depth(depth);
    if color == ColorType::Indexed {
      enc.set_palette(&b"abcdef"[..]);
    }

    if !trns.is_empty() {
      enc.set_trns(trns);
    }

    let mut writer = enc.write_header().unwrap();
    writer.write_image_data(data).unwrap();
    writer.finish().unwrap();
    out
  }

  #[test]
  fn decode() {
    use BitDepth::*;
    use ColorType::*;

    let cases: &[Case] = &[
      (Indexed, Eight, &[0, 1], &[0], b"abc\0def\xff"),
      (Indexed, Two, &[0b0001_0000], &[], b"abc\xffdef\xff"),
      (Grayscale, Eight, &[7, 200], &[], &[7, 7, 7, 255, 200, 200, 200, 255]),
      (Grayscale, One, &[0b1000_0000], &[], &[255, 255, 255, 255, 0, 0, 0, 255]),
      (
        Grayscale,
        Sixteen,
        &[0x12, 0x34, 0xff, 0],
        &[],
        &[0x12, 0x12, 0x12, 255, 255, 255, 255, 255],
      ),
      (GrayscaleAlpha, Eight, &[7, 9, 200, 0], &[], &[7, 7, 7, 9, 200, 200, 200, 0]),
      (Rgb, Eight, &[1, 2, 3, 4, 5, 6], &[], &[1, 2, 3, 255, 4, 5, 6, 255]),
      (Rgb, Eight, &[1, 2, 3, 4, 5, 6], &[0, 4, 0, 5, 0, 6], &[1, 2, 3, 255, 4, 5, 6, 0]),
      (Rgba, Eight, &[1, 2, 3, 4, 5, 6, 7, 8], &[], &[1, 2, 3, 4, 5, 6, 7, 8]),
      (
        Rgba,
        Sixteen,
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        &[],
        &[1, 3, 5, 7, 9, 11, 13, 15],
      ),
    ];

    for case in cases {
      let (color, depth, .., rgba) = case;
      let image = Image::decode(&png(case)).unwrap();
      assert_eq!((image.w, image.h), (2, 1), "{color:?} {depth:?}");
      assert_eq!(image.px, *rgba, "{color:?} {depth:?}");
    }

    let err = Image::decode(b"GIF89a").unwrap_err().to_string();
    assert!(err.contains("signature"), "{err}");
  }

  #[test]
  fn interlaced() {
    // A 2×2 PNG in Adam7, which the crate can't write: pass 1 holds the top left pixel, pass 6
    // the top right, pass 7 the bottom row.
    let data = concat!(
      "89504e470d0a1a0a0000000d494844520000000200000002080600000105b13db20000001b4944415478da6360",
      "64626661606563e760e0e4e2e6e1e5e31700000371008973658d4f0000000049454e44ae426082",
    );

    let data: Vec<u8> = (0..data.len())
      .step_by(2)
      .map(|i| u8::from_str_radix(&data[i..i + 2], 16).unwrap())
      .collect();
    assert_eq!(Image::decode(&data).unwrap(), Image { w: 2, h: 2, px: (1..=16).collect() });
  }

  #[test]
  fn read() {
    let dir = Temp::new("read");
    let (path, image) = (dir.0.join("a.png"), Image { w: 3, h: 1, px: (0..12).collect() });
    fs::write(&path, image.encode().unwrap()).unwrap();
    assert_eq!(Image::read(&path).unwrap(), image);

    let missing = dir.0.join("missing.png");
    let err = Image::read(&missing).unwrap_err().to_string();
    assert!(err.starts_with(&format!("{}: ", missing.display())), "{err}");
  }

  #[test]
  fn encode() {
    let image = Image { w: 4, h: 2, px: (0..32).collect() };
    let data = image.encode().unwrap();
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(Image::decode(&data).unwrap(), image);
  }

  #[test]
  fn resized() {
    // Weighted by alpha: the transparent pixel's colour counts for nothing, its alpha for a
    // quarter.
    let px = vec![255, 0, 0, 255, 7, 7, 7, 0, 0, 255, 0, 255, 0, 0, 255, 255];
    let image = Image { w: 2, h: 2, px: px.clone() };
    let out = image.resized(&[1, 2]);
    assert_eq!(out, [Image { w: 1, h: 1, px: vec![85, 85, 85, 191] }, Image { w: 2, h: 2, px }]);

    // Opaque greys `x + 4y` in 2×2 boxes: each box's mean ends in .5, rounded up.
    let px = (0..16).flat_map(|v| [v, v, v, 255]).collect();
    let out = Image { w: 4, h: 4, px }.resized(&[2]);
    assert_eq!(out[0].px, [3, 3, 3, 255, 5, 5, 5, 255, 11, 11, 11, 255, 13, 13, 13, 255]);

    // 3 to 2: the boxes are 2×2, 1×2, 2×1 and 1×1 pixels; alpha their mean, the colour as it is.
    let alpha = [0, 255, 100, 255, 0, 51, 10, 20, 30];
    let px = alpha.iter().flat_map(|&a| [200, 100, 50, a]).collect();
    let out = Image { w: 3, h: 3, px }.resized(&[2]);
    let rgb = |a| [200, 100, 50, a];
    assert_eq!(out[0].px, [rgb(128), rgb(76), rgb(15), rgb(30)].concat());

    // Uneven ratios against each box summed over its own rows and columns, `ceil(j·w/n)` on.
    let image = icon(300);
    for n in [7, 24, 48, 100, 299] {
      let edge = |j: usize| (j * 300).div_ceil(n);
      let mut px = vec![];
      for (k, j) in (0..n).flat_map(|k| (0..n).map(move |j| (k, j))) {
        let mut sum = [0u64; 4];
        for (x, y) in
          (edge(j)..edge(j + 1)).flat_map(|x| (edge(k)..edge(k + 1)).map(move |y| (x, y)))
        {
          let p = &image.px[(y * 300 + x) * 4..][..4];
          (0..3).for_each(|c| sum[c] += p[c] as u64 * p[3] as u64);
          sum[3] += p[3] as u64;
        }

        let (a, count) = (sum[3], ((edge(k + 1) - edge(k)) * (edge(j + 1) - edge(j))) as u64);
        px.extend(sum[..3].iter().map(|&v| if a > 0 { ((2 * v + a) / (2 * a)) as u8 } else { 0 }));
        px.push(((2 * a + count) / (2 * count)) as u8);
      }

      assert!(image.resized(&[n])[0].px == px, "{n} px");
    }

    let clear = Image { w: 2, h: 2, px: [9, 9, 9, 0].repeat(4) };
    assert_eq!(clear.resized(&[1])[0].px, [0, 0, 0, 0]);
  }

  #[test]
  fn icons_checks() {
    let image = Image { w: 2, h: 2, px: [1, 2, 3, 255].repeat(4) };
    let out = icons(&image, &[2, 1, 2], false).unwrap();
    assert_eq!(out.keys().copied().collect::<Vec<_>>(), [1, 2]);
    assert_eq!(out[&1].px, [1, 2, 3, 255]);
    assert!(icons(&image, &[], false).unwrap().is_empty());

    let err =
      |icon: &Image, sizes: &[usize], crop| icons(icon, sizes, crop).unwrap_err().to_string();

    assert_eq!(err(&image, &[1, 3], false), "the icon is 2×2, smaller than 3 px");
    let wide = Image { w: 4, h: 2, px: [1, 2, 3, 255].repeat(8) };
    assert_eq!(err(&wide, &[1], false), "the icon is 4×2, not square");
    assert_eq!(icons(&wide, &[2], true).unwrap()[&2].w, 2);

    let tile = "the icon's visible tile is 320×320, smaller than 400 px";
    assert_eq!(err(&icon(400), &[16, 400], true), tile);
    assert_eq!(err(&Image { w: 1, h: 1, px: vec![0; 4] }, &[1], true), "the icon is transparent");
  }

  #[test]
  fn cropped() {
    // Visible at x 1..=3 and y 4..=5 of 8×6, an alpha of 8 ignored: a side of 3 from (1, 4), its
    // last row below the image.
    let mut image = Image { w: 8, h: 6, px: vec![0; 8 * 6 * 4] };
    for (x, y) in [(1, 4), (2, 4), (3, 4), (1, 5), (3, 5)] {
      image.px[(y * 8 + x) * 4..][..4].copy_from_slice(&[x as u8, y as u8, 0, 255]);
    }

    image.px[7 * 4 + 3] = 8;
    let tile = image.cropped().unwrap();
    let (a, b, c) = ([1, 4, 0, 255], [2, 4, 0, 255], [3, 4, 0, 255]);
    let (d, e, none) = ([1, 5, 0, 255], [3, 5, 0, 255], [0; 4]);
    assert_eq!((tile.w, tile.h), (3, 3));
    assert_eq!(tile.px, [a, b, c, d, none, e, none, none, none].concat());

    // One column at the left edge, y 0..=3, its last pixel at an alpha of 9: a side of 4 from
    // (-2, 0), so the column lands in the tile's third.
    let mut image = Image { w: 5, h: 5, px: vec![0; 5 * 5 * 4] };
    for y in 0..4 {
      image.px[y * 5 * 4..][..4].copy_from_slice(&[y as u8, 0, 0, if y == 3 { 9 } else { 255 }]);
    }

    let tile = image.cropped().unwrap();
    assert_eq!((tile.w, tile.h), (4, 4));
    for (i, p) in tile.px.as_chunks::<4>().0.iter().enumerate() {
      let (x, y) = (i % 4, i / 4);
      let want = if x == 2 { [y as u8, 0, 0, if y == 3 { 9 } else { 255 }] } else { [0; 4] };
      assert_eq!(*p, want, "({x}, {y})");
    }

    // The test icon: the tile 320 px across from (40, 80), centred down from (40, 60).
    let icon = icon(400);
    let tile = icon.cropped().unwrap();
    assert_eq!((tile.w, tile.h), (320, 320));
    assert_eq!(tile.px[..20 * 320 * 4], vec![0; 20 * 320 * 4]);
    for y in 20..300 {
      let (at, from) = (y * 320 * 4, ((y + 60) * 400 + 40) * 4);
      assert_eq!(tile.px[at..at + 320 * 4], icon.px[from..from + 320 * 4], "row {y}");
    }

    let faint = Image { w: 2, h: 2, px: [0, 0, 0, 8].repeat(4) };
    assert_eq!(faint.cropped().unwrap_err().to_string(), "the icon is transparent");
  }

  #[test]
  fn icns_layout() {
    let images = icons(&icon(1024), &ICNS.map(|(_, n)| n), false).unwrap();
    let entries = icns_entries(&icns(&images).unwrap());
    assert_eq!(entries.iter().map(|e| e.0).collect::<Vec<_>>(), ICNS.map(|(kind, _)| *kind));

    for ((kind, body, image), (_, n)) in entries.iter().zip(ICNS) {
      let name = String::from_utf8_lossy(kind);
      let argb = matches!(kind, b"ic04" | b"ic05");
      assert_eq!(body.starts_with(b"ARGB"), argb, "{name}");
      match argb {
        true => close(image, &images[&n], &name),
        false => assert_eq!(*image, images[&n], "{name}"),
      }
    }

    // Premultiplied: (200, 100, 0) at an alpha of 128 is (100, 50, 0). The alpha plane is two runs
    // of 128 from byte 20, then each colour's.
    let mut images = images;
    images.insert(16, Image { w: 16, h: 16, px: [200, 100, 0, 128].repeat(256) });
    let data = icns(&images).unwrap();
    assert_eq!(&data[16..23], b"ARGB\x7f\x80\x80");
    assert_eq!((&data[276..281], data[537], data[795]), (&b"\x80\x80\x7f\x64\x64"[..], 50, 0));

    let err = icns(&BTreeMap::new()).unwrap_err().to_string();
    assert_eq!(err, "no 16 px image for the .icns");
  }

  #[test]
  fn ico_layout() {
    let images = icons(&icon(400), &ICO, true).unwrap();
    let pngs: BTreeMap<_, _> = images.iter().map(|(&n, x)| (n, x.encode().unwrap())).collect();
    let entries = ico_entries(&ico(&pngs).unwrap());
    assert_eq!(entries.len(), ICO.len());

    for ((head, more, image), n) in entries.iter().zip(ICO) {
      assert_eq!((*head, *more), ([(n % 256) as u8, (n % 256) as u8, 0, 0], [1, 32]));
      assert_eq!(*image, images[&n], "{n} px");
    }

    let big = BTreeMap::from([(512, vec![])]);
    let err = "a 512 px icon, above the 256 px an icon directory holds";
    assert_eq!(ico(&big).unwrap_err().to_string(), err);
    assert_eq!(group_icon(&big, 1).unwrap_err().to_string(), err);
  }

  #[test]
  fn group_icon_layout() {
    let pngs = BTreeMap::from([(16, vec![1; 10]), (256, vec![2; 300])]);
    let data = group_icon(&pngs, 7).unwrap();
    let le16 = |at: usize| u16::from_le_bytes(data[at..at + 2].try_into().unwrap());
    assert_eq!(data.len(), 6 + 2 * 14);
    assert_eq!([le16(0), le16(2), le16(4)], [0, 1, 2]);

    // Each entry: the sizes, two zeros, planes and bits per pixel, the PNG's length, its id.
    for (i, (n, len, id)) in [(16, 10, 7), (0, 300, 8)].into_iter().enumerate() {
      let at = 6 + 14 * i;
      assert_eq!(data[at..at + 4], [n, n, 0, 0], "entry {i}");
      assert_eq!([le16(at + 4), le16(at + 6)], [1, 32], "entry {i}");
      assert_eq!(u32::from_le_bytes(data[at + 8..at + 12].try_into().unwrap()), len, "entry {i}");
      assert_eq!(le16(at + 12), id, "entry {i}");
    }

    let err = group_icon(&pngs, u16::MAX).unwrap_err().to_string();
    assert_eq!(err, "the icon's RT_ICON ids pass 65535");
  }

  /// The system's own reader: iconutil takes our `.icns` apart into the iconset's PNGs, each the
  /// image we gave it.
  #[test]
  #[cfg(target_os = "macos")]
  fn iconutil() {
    let dir = Temp::new("iconutil");
    let images = icons(&icon(1024), &ICNS.map(|(_, n)| n), false).unwrap();
    fs::write(dir.0.join("icon.icns"), icns(&images).unwrap()).unwrap();
    let set = dir.0.join("icon.iconset");
    let out = std::process::Command::new("iconutil")
      .args(["-c", "iconset", "-o"])
      .args([&set, &dir.0.join("icon.icns")])
      .output()
      .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let mut names: Vec<_> = fs::read_dir(&set).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    let sizes = [16, 32, 128, 256, 512];
    let mut want: Vec<_> = sizes.iter().map(|n| (format!("icon_{n}x{n}.png"), *n)).collect();
    want.extend(sizes.iter().map(|n| (format!("icon_{n}x{n}@2x.png"), 2 * n)));
    want.sort();
    let want_names: Vec<std::ffi::OsString> = want.iter().map(|(name, _)| name.into()).collect();
    assert_eq!(names, want_names);

    for (name, n) in want {
      close(&Image::read(&set.join(&name)).unwrap(), &images[&n], &name);
    }
  }

  // MARK: Helpers

  /// A new folder in the temp dir, removed on drop.
  struct Temp(std::path::PathBuf);

  impl Temp {
    fn new(name: &str) -> Self {
      let dir = std::env::temp_dir().join(format!("shipo-cli-{}-image-{name}", std::process::id()));
      let _ = fs::remove_dir_all(&dir);
      fs::create_dir_all(&dir).unwrap();
      Self(dir)
    }
  }

  impl Drop for Temp {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  /// Checks `ours` is the image `want`, its alpha exact, its colours exact where opaque and close
  /// where not: a colour premultiplied by alpha keeps `c·a/255` rounded, so read back straight it
  /// is off by up to about `128/a`.
  fn close(ours: &Image, want: &Image, name: &str) {
    assert_eq!((ours.w, ours.h), (want.w, want.h), "{name}");

    let pixels = ours.px.as_chunks::<4>().0.iter().zip(want.px.as_chunks::<4>().0);
    for (i, (p, q)) in pixels.enumerate() {
      let a = q[3] as i32;
      let off = (0..3).map(|c| (p[c] as i32 - q[c] as i32).abs()).max().unwrap();
      let fits = p[3] == q[3] && (a == 0 || off <= if a == 255 { 0 } else { 1 + 128 / a });
      assert!(fits, "{name}: pixel {i} is {p:?}, not {q:?}");
    }
  }

  /// An `n` px test icon: a transparent margin, a tile off centre and not square (x 10–90 %, y
  /// 20–90 %) in gradients, with a hole that keeps its colour and a band of rising alpha.
  fn icon(n: usize) -> Image {
    let mut px = vec![0; n * n * 4];
    for (i, p) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
      let (x, y) = (i % n, i / n);
      let (fx, fy) = (x * 10 / n, y * 10 / n);
      let colour = [(x * 255 / n) as u8, (y * 255 / n) as u8, 128];
      *p = match (fx, fy) {
        (1..=8, 2..=8) if fx == 4 && fy == 4 => [255, 0, 255, 0],
        (1..=8, 7) => [40, 90, 200, (x * 255 / n) as u8],
        (1..=8, 2..=8) => [colour[0], colour[1], colour[2], 255],
        _ => [0; 4],
      };
    }

    Image { w: n, h: n, px }
  }

  /// An `.icns`'s entries, its type and its image, checked that they fill the file: an `ARGB` by
  /// its RLE, the rest by PNG.
  fn icns_entries(data: &[u8]) -> Vec<([u8; 4], Vec<u8>, Image)> {
    let be = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(&data[..4], b"icns");
    assert_eq!(be(4), data.len());

    let (mut at, mut out) = (8, vec![]);
    while at < data.len() {
      let (kind, len) = (data[at..at + 4].try_into().unwrap(), be(at + 4));
      assert!(len >= 8 && at + len <= data.len(), "{kind:?} at {at}: {len} bytes");
      let body = &data[at + 8..at + len];
      let image = match body.strip_prefix(b"ARGB") {
        Some(rle) => argb(rle),
        None => Image::decode(body).unwrap(),
      };

      out.push((kind, body.to_vec(), image));
      at += len;
    }

    assert_eq!(at, data.len());
    out
  }

  /// The image of an `ARGB` entry's runs, straight: a byte below 128 is a literal run of it plus
  /// one bytes, else one byte repeated it less 125 times; the planes A, R, G and B one after
  /// another, the colours premultiplied by alpha.
  fn argb(mut rle: &[u8]) -> Image {
    let mut planes = vec![];
    while let [head, rest @ ..] = rle {
      if *head < 128 {
        planes.extend_from_slice(&rest[..*head as usize + 1]);
        rle = &rest[*head as usize + 1..];
      } else {
        planes.extend(std::iter::repeat_n(rest[0], *head as usize - 125));
        rle = &rest[1..];
      }
    }

    let count = planes.len() / 4;
    let n = (1..=count).find(|n| n * n == count).unwrap();
    let px = (0..count).flat_map(|i| {
      let a = planes[i] as u32;
      let straight =
        |v: u8| if a > 0 { ((2 * 255 * v as u32 + a) / (2 * a)).min(255) as u8 } else { 0 };
      [1, 2, 3].map(|c| straight(planes[c * count + i])).into_iter().chain([a as u8])
    });

    Image { w: n, h: n, px: px.collect() }
  }

  /// An `.ico`'s entries, the header's six fields and the image, checked that its offsets and
  /// lengths fill the file in order.
  fn ico_entries(data: &[u8]) -> Vec<([u8; 4], [u16; 2], Image)> {
    let le16 = |at: usize| u16::from_le_bytes(data[at..at + 2].try_into().unwrap());
    let le32 = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!((le16(0), le16(2)), (0, 1));

    let count = le16(4) as usize;
    let mut next = 6 + 16 * count;
    let mut out = vec![];
    for i in 0..count {
      let at = 6 + 16 * i;
      let (len, offset) = (le32(at + 8), le32(at + 12));
      assert_eq!(offset, next, "entry {i}");
      next += len;
      let head = data[at..at + 4].try_into().unwrap();
      let image = Image::decode(&data[offset..offset + len]).unwrap();
      assert_eq!((image.w % 256, image.h % 256), (data[at] as usize, data[at + 1] as usize));
      out.push((head, [le16(at + 4), le16(at + 6)], image));
    }

    assert_eq!(next, data.len());
    out
  }
}
