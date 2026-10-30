# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""Writes src/cli/unicode.rs: the tables HFS+ decomposes names by, from Unicode 3.2 as TN1150 has it.

A name on HFS+ is stored decomposed (NFD) by Unicode 3.2's canonical decompositions, but for
U+2000–U+2FFF, U+F900–U+FAFF and U+2F800–U+2FAFF, which stay as they are. Python keeps 3.2's
data as `unicodedata.ucd_3_2_0`. Hangul syllables decompose by formula, so they are not listed.

uv run scripts/hfs-unicode.py
"""

import platform
import unicodedata
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "src/cli/unicode.rs"
UCD = unicodedata.ucd_3_2_0


def kept(c: int) -> bool:
  """Whether HFS+ leaves `c` as it is though Unicode decomposes it."""
  return 0x2000 <= c < 0x3000 or 0xF900 <= c < 0xFB00 or 0xAC00 <= c <= 0xD7A3


def words(values: list[int], width: int) -> str:
  """`values` as hex Rust literals, `width` to a line."""
  lines = [values[i : i + width] for i in range(0, len(values), width)]
  return "\n".join("  " + " ".join(f"0x{v:04X}," for v in ln) for ln in lines)


def main() -> None:
  chars = [c for c in range(0x10000) if not 0xD800 <= c < 0xE000]
  nfd = []
  for c in chars:
    parts = UCD.normalize("NFD", chr(c))
    if parts != chr(c) and not kept(c):
      units = [ord(p) for p in parts]
      assert len(units) <= 4, f"U+{c:04X} decomposes to {len(units)} units"
      nfd += [c, *units, *[0] * (4 - len(units))]

  ccc = []
  for c in chars:
    if UCD.combining(chr(c)):
      ccc += [c, UCD.combining(chr(c))]

  OUT.write_text(f"""// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Unicode 3.2 tables HFS+ decomposes names by (TN1150), written by `scripts/hfs-unicode.py`
//! from Python {platform.python_version()}'s `unicodedata.ucd_3_2_0` ({UCD.unidata_version}).

/// The characters that decompose, by code: each the character, then its full decomposition in up
/// to 4 units, 0 after the last.
#[rustfmt::skip]
pub const NFD: [u16; {len(nfd)}] = [
{words(nfd, 10)}
];

/// The characters of a combining class other than 0, by code: each the character, then its class.
#[rustfmt::skip]
pub const CCC: [u16; {len(ccc)}] = [
{words(ccc, 12)}
];
""")


if __name__ == "__main__":
  main()
