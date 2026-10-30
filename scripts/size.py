# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""Lines per part: code, docs, comments and inline tests, apart.

A part is a set of files (PARTS) with a budget of code lines; examples/ and tests/ are one row
each, and generated files (GENERATED) one row with no budget. Code is every line that is not
blank, a comment, a doc comment or inside a `#[cfg(test)]` item. With a git ref, a column shows
how code moved since it.
`--map` lists the files with the first sentence of each one's module doc (`//!`): what it does.
`--check` sums the code per part against its budget and fails when one is over, when a budget
is not a whole number of hundreds, or when a file is in no part; with `SIZE_WARN` set (CI on
`main`, where two PRs that each fit can merge into one over) it only warns.

uv run scripts/size.py [--files | --map | --check] [--vs <ref>]
"""

import argparse
import os
import re
import subprocess
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIRS = tuple(d for d in ("src", "cli/src", "examples", "tests") if (ROOT / d).is_dir())
EXTS = (".rs",)
KINDS = ("code", "docs", "comments", "tests", "blank")
# The parts, each with its files (`*` stays within a directory, `{a,b}` is either) and the code
# lines it may have. A budget is whole hundreds (STEP): a change that doesn't fit is written
# tighter, or the budget goes up by a hundred, a decision recorded in its PR; never by the few
# lines one change needs. A new file joins a part on purpose: `--check` fails until it does.
STEP = 100
PARTS = {
  # The library: the feed, the download and the install, the public API.
  "library": (800, ("src/*.rs",)),
  # The library's OS code: each OS, the shared Unix code and the dispatch.
  "library/platform": (500, ("src/platform/*.rs",)),
  # The packer's commands, config, tool fetching, the app tree, the feed and the icons.
  "packer": (1300, ("cli/src/{main,config,tool,tree,feed,image}.rs",)),
  # The .app bundle and the DMG with its HFS+ volume, written ourselves.
  "packer/macos": (1600, ("cli/src/{macos,plist,ds_store,dmg,hfs,btree,udif}.rs",)),
  # The setup and the PE resources.
  "packer/windows": (700, ("cli/src/{windows,pe}.rs",)),
  # The AppImage with its squashfs, and the .deb.
  "packer/linux": (600, ("cli/src/{linux,deb,squashfs}.rs",)),
}
# Files a script writes (scripts/hfs-unicode.py: the HFS+ decomposition tables): no budget.
GENERATED = ("cli/src/unicode.rs",)


def matches(pattern: str, path: str) -> bool:
  """Whether a path is one of a pattern's files.

  >>> [matches("src/*.rs", p) for p in ("src/lib.rs", "src/platform/macos.rs", "cli/src/main.rs")]
  [True, False, False]
  >>> [matches("cli/src/{pe,windows}.rs", p) for p in ("cli/src/pe.rs", "cli/src/windows.rs", "cli/src/pex.rs")]
  [True, True, False]
  """
  rx = re.escape(pattern).replace(r"\*", "[^/]*")
  rx = re.sub(r"\\\{(.*?)\\}", lambda m: "(?:" + m.group(1).replace(",", "|") + ")", rx)
  return re.fullmatch(rx, path) is not None


def part(path: str) -> str | None:
  """The part a file's code counts against; examples, tests and generated files have none.

  >>> [part(p) for p in ("src/lib.rs", "src/platform/unix.rs", "cli/src/main.rs", "cli/src/hfs.rs")]
  ['library', 'library/platform', 'packer', 'packer/macos']
  >>> [part(p) for p in ("cli/src/pe.rs", "cli/src/deb.rs", "cli/src/unicode.rs", "cli/src/new.rs")]
  ['packer/windows', 'packer/linux', None, None]
  """
  for name, (_, patterns) in PARTS.items():
    if any(matches(p, path) for p in patterns):
      return name

  return None


def module(path: str, files: bool) -> str:
  """The row a file's lines count in: its part, or the file itself when in none.

  >>> [module(p, False) for p in ("src/feed.rs", "cli/src/feed.rs", "examples/update.rs")]
  ['library', 'packer', 'examples']
  >>> [module(p, False) for p in ("cli/src/unicode.rs", "cli/src/new.rs")]
  ['generated', 'cli/src/new.rs']
  """
  if files:
    return path

  if path in GENERATED:
    return "generated"

  if path.split("/")[0] in ("examples", "tests"):
    return path.split("/")[0]

  return part(path) or path


def check(files: dict[str, str]) -> bool:
  """Prints each part's code against its budget; whether all are within and every file has one."""
  used = defaultdict(int)
  loose = []
  for path, text in files.items():
    if (p := part(path)) is not None:
      used[p] += count(text)["code"]
    elif module(path, False) == path:
      loose.append(path)

  for path in loose:
    print(f"{path}: in no part: add it to one in PARTS (scripts/size.py)")

  odd = [p for p, (limit, _) in PARTS.items() if limit % STEP]
  for p in odd:
    print(f"{p}: budget {PARTS[p][0]} is not whole hundreds: raise it by {STEP}, not by a few lines")

  ok = not odd and not loose
  for p, (limit, _) in PARTS.items():
    over = used[p] > limit
    print(f"{p:<18} {used[p]:>6} / {limit:<6} {'over' if over else 'ok'}")
    ok &= not over

  return ok


def braces(line: str) -> int:
  """How much a line deepens the braces, outside strings, chars and comments.

  >>> [braces(s) for s in ("fn a() {", '}', 'let s = "{";', "let c = '}'; // {")]
  [1, -1, 0, 0]
  """
  line = re.sub(r'"(\\.|[^"\\])*"', "", line.split("//")[0])
  line = re.sub(r"'(\\.|[^'\\])'", "", line)
  return line.count("{") - line.count("}")


def count(text: str) -> dict[str, int]:
  """The lines of a file by kind.

  >>> count("//! A file.\\n\\nfn a() {} // a\\n// b\\n#[cfg(test)]\\nmod tests {\\n  #[test]\\n}\\n")
  {'code': 1, 'docs': 1, 'comments': 1, 'tests': 4, 'blank': 1}
  """
  n = dict.fromkeys(KINDS, 0)
  test, depth = False, 0

  for raw in text.splitlines():
    line = raw.strip()
    if not test and line.startswith("#[cfg(test)]"):
      test, depth = True, 0

    if test:
      n["tests"] += 1
      depth += braces(line)
      if depth == 0 and ("}" in line or line.endswith(";")):
        test = False
      continue

    if not line:
      n["blank"] += 1
    elif line.startswith(("///", "//!")):
      n["docs"] += 1
    elif line.startswith("//"):
      n["comments"] += 1
    else:
      n["code"] += 1

  return n


def summary(text: str) -> str:
  """The first sentence of a file's leading `//!` doc.

  >>> summary("// Copyright\\n//! Writes a DMG. Then more.\\n//! And more.\\nuse x;\\n")
  'Writes a DMG'
  """
  doc = []
  for raw in text.splitlines():
    line = raw.strip()
    if line.startswith("//!"):
      doc.append(line[3:].strip())
    elif doc or (line and not line.startswith("//")):
      break

  first = re.split(r"(?<=[.])\s", " ".join(doc).strip(), maxsplit=1)[0]
  return first.rstrip(".") if len(first) <= 110 else first[:109].rstrip() + "…"


def files_now() -> dict[str, str]:
  out = {}
  for d in DIRS:
    for p in sorted((ROOT / d).rglob("*")):
      if p.suffix in EXTS:
        out[p.relative_to(ROOT).as_posix()] = p.read_text()

  return out


def files_at(ref: str) -> dict[str, str]:
  git = ["git", "-C", str(ROOT)]
  names = subprocess.run([*git, "ls-tree", "-r", "--name-only", ref, *DIRS], capture_output=True, text=True, check=True).stdout
  out = {}
  for name in names.split():
    if name.endswith(EXTS):
      out[name] = subprocess.run([*git, "show", f"{ref}:{name}"], capture_output=True, text=True, check=True).stdout

  return out


def table(files: dict[str, str], by_file: bool) -> dict[str, dict[str, int]]:
  rows = defaultdict(lambda: dict.fromkeys(KINDS, 0))
  for path, text in files.items():
    row = rows[module(path, by_file)]
    for k, v in count(text).items():
      row[k] += v

  return rows


def main():
  ap = argparse.ArgumentParser(description="Lines per part.")
  ap.add_argument("--files", action="store_true", help="one row per file")
  ap.add_argument("--map", action="store_true", help="one row per file with what it does")
  ap.add_argument("--vs", metavar="REF", help="show the code change since this git ref")
  ap.add_argument("--check", action="store_true", help="fail when a part is over its budget or a file in none")
  args = ap.parse_args()

  files = files_now()
  if args.check:
    ok = check(files)
    if not ok and os.environ.get("SIZE_WARN"):
      print("::warning::a part is over its code budget or a file is in none; fix it in the next PR")

    raise SystemExit(0 if ok or os.environ.get("SIZE_WARN") else 1)

  if args.map:
    for path in sorted(files, key=lambda p: (p.count("/"), p)):
      print(f"{path:<28} {count(files[path])['code']:>5}  {summary(files[path])}")

    return

  rows = table(files, args.files)
  base = table(files_at(args.vs), args.files) if args.vs else None
  names = sorted(set(rows) | set(base or {}), key=lambda m: -rows[m]["code"] if m in rows else 0)
  total = {k: sum(r[k] for r in rows.values()) for k in KINDS}

  w = max(len("module"), *(len(m) for m in names))
  head = f"{'module':<{w}} {'code':>7} {'%':>4} {'docs':>6} {'comm':>6} {'tests':>6} {'blank':>6}"
  print(head + (f" {'vs ' + args.vs:>10}" if base else ""))
  print("-" * (len(head) + (11 if base else 0)))

  for m in [*names, None]:
    r = total if m is None else rows.get(m, dict.fromkeys(KINDS, 0))
    if m is None:
      print("-" * (len(head) + (11 if base else 0)))

    pct = 100 * r["code"] / max(total["code"], 1)
    line = f"{m or 'total':<{w}} {r['code']:>7} {pct:>4.0f} {r['docs']:>6} {r['comments']:>6} {r['tests']:>6} {r['blank']:>6}"
    if base is not None:
      was = sum(b["code"] for b in base.values()) if m is None else base.get(m, {"code": 0})["code"]
      line += f" {r['code'] - was:>+10}"

    print(line)


if __name__ == "__main__":
  main()
