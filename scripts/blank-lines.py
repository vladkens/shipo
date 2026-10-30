# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""Checks that a statement after an if / for / while / loop / match block starts after a blank line.

rustfmt keeps blank lines but never adds them, so this finds the missing ones: a block's closing
brace followed, at the same indentation, by another statement. `--fix` inserts the blank lines.

uv run scripts/blank-lines.py [--fix] [paths...]   (default: src setup/src examples tests)
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OPEN = re.compile(r"^(?:let\b.*=\s*)?(?:if|for|while|loop|match)\b.*\{$")
CLOSE = re.compile(r"^\}[;,]?$")
NOT_STATEMENT = ("}", ")", "]", ".", "else", "//", "#[")


def missing(lines: list[str]) -> list[int]:
  """Indexes of the lines that need a blank line above them."""
  opened: list[int] = []
  out = []

  for i, raw in enumerate(lines):
    text = raw.strip()
    indent = len(raw) - len(raw.lstrip())
    if OPEN.match(text):
      opened.append(indent)
      continue

    if not (opened and indent == opened[-1] and CLOSE.match(text)):
      continue

    opened.pop()
    if i + 1 >= len(lines):
      continue

    after = lines[i + 1]
    if after.strip() and len(after) - len(after.lstrip()) == indent and not after.strip().startswith(NOT_STATEMENT):
      out.append(i + 1)

  return out


def main():
  args = sys.argv[1:]
  fix = "--fix" in args
  roots = [Path(a) for a in args if a != "--fix"] or [ROOT / d for d in ("src", "setup/src", "examples", "tests")]
  found = 0

  # Not the example apps' build folders, examples/<app>/target.
  files = (p for r in roots for p in ([r] if r.is_file() else r.rglob("*.rs")) if "target" not in p.relative_to(r).parts)
  for path in sorted(files):
    lines = path.read_text().splitlines(keepends=True)
    hits = missing(lines)
    found += len(hits)
    for i in hits:
      print(f"{path}:{i + 1}: {lines[i].strip()}")

    if fix and hits:
      for i in reversed(hits):
        lines.insert(i, "\n")

      path.write_text("".join(lines))

  if found and not fix:
    print(f"{found} statements need a blank line above them; `uv run scripts/blank-lines.py --fix` adds them")
    sys.exit(1)


if __name__ == "__main__":
  main()
