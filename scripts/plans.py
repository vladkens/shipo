# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""Checks that each plan is where its state says: open in `docs/plans/`, finished in `completed/`.

A plan in `docs/plans/` with no unchecked box left is finished but not moved; one in
`docs/plans/completed/` needs a `Done: yyyy-mm-dd` line on top and a `## Result` section.
Prints one line per problem and exits 1; silent otherwise.

uv run scripts/plans.py
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLANS = ROOT / "docs" / "plans"


def lines(text: str) -> list[str]:
  """A file's lines outside fenced code blocks."""
  out, fenced = [], False
  for line in text.splitlines():
    if line.lstrip().startswith("```"):
      fenced = not fenced
    elif not fenced:
      out.append(line)

  return out


def is_open(text: str) -> bool:
  """Whether a plan still has an unchecked box.

  >>> [is_open(s) for s in ("# A\\n- [x] a\\n  - [ ] b\\n", "# A\\n- [x] a\\n", "```\\n- [ ] a\\n```\\n")]
  [True, False, False]
  """
  return any(re.match(r"\s*[-*] \[ \]", line) for line in lines(text))


def problems(text: str) -> list[str]:
  """What a completed plan lacks: the `Done:` line on top (before the first section) or the result.

  >>> problems("Done: 2026-10-30\\n# A\\n## Tasks\\n## Result\\nIt works.\\n")
  []
  >>> problems("# A\\n\\nDone: 2026-10-30\\n\\n## Result\\n")
  []
  >>> problems("# A\\n## Tasks\\nDone: 2026-10-30\\n")
  ['no `Done: yyyy-mm-dd` line on top', 'no `## Result` section']
  """
  rows = lines(text)
  head = next((i for i, line in enumerate(rows) if line.startswith("## ")), len(rows))
  out = []
  if not any(re.fullmatch(r"Done: \d{4}-\d{2}-\d{2}", line.strip()) for line in rows[:head]):
    out.append("no `Done: yyyy-mm-dd` line on top")

  if not any(re.fullmatch(r"## Result\s*", line) for line in rows):
    out.append("no `## Result` section")

  return out


def main():
  found = []
  for path in sorted(PLANS.glob("*.md")):
    if not is_open(path.read_text()):
      found.append(f"{path.relative_to(ROOT)}: no unchecked box left: add `Done:` and `## Result`, move it to docs/plans/completed/")

  for path in sorted((PLANS / "completed").glob("*.md")):
    for problem in problems(path.read_text()):
      found.append(f"{path.relative_to(ROOT)}: {problem}")

  for line in found:
    print(line)

  if found:
    sys.exit(1)


if __name__ == "__main__":
  main()
