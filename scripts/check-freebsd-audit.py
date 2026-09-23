#!/usr/bin/env python3
"""Fail when the FreeBSD platform audit drifts from Rust source matches.

Audit rows are keyed by file path plus the trimmed source text of the matched
line, not by line number, so unrelated edits that shift lines do not break the
check. A file with several identical matched lines needs that many rows.
"""

from __future__ import annotations

import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AUDIT = ROOT / "docs" / "platforms" / "freebsd-audit.md"
PATTERN = re.compile(r"target_os|target_family|cfg!\(|std::env::consts::OS|systemd|launchd|notify-send|osascript")
# | `path/to/file.rs` | `code` | ...   (code may use a longer backtick fence)
ROW = re.compile(r"^\| `([^`]+\.rs)` \| (`+) ?(.*?) ?\2 \|")

Key = tuple[str, str]


def normalize(code: str) -> str:
    return " ".join(code.split())


def encode(code: str) -> str:
    """Render source text as a markdown table cell code span."""
    code = normalize(code).replace("|", "\\|")
    fence = "`" * (max((len(run) for run in re.findall(r"`+", code)), default=0) + 1)
    pad = " " if "`" in code else ""
    return f"{fence}{pad}{code}{pad}{fence}"


def source_hits() -> Counter[Key]:
    result = subprocess.run(
        ["git", "ls-files", "*.rs"], cwd=ROOT, text=True, capture_output=True, check=True
    )
    hits: Counter[Key] = Counter()
    for relative in result.stdout.splitlines():
        path = ROOT / relative
        for line in path.read_text(encoding="utf-8").splitlines():
            if PATTERN.search(line):
                hits[(relative, normalize(line))] += 1
    return hits


def audit_hits() -> Counter[Key]:
    rows: Counter[Key] = Counter()
    for line in AUDIT.read_text(encoding="utf-8").splitlines():
        if match := ROW.match(line):
            relative, _, code = match.groups()
            rows[(relative, normalize(code.replace("\\|", "|")))] += 1
    return rows


def show(entries: Counter[Key]) -> list[str]:
    return [
        f"{relative}: {code}" + (f"  (x{count})" if count > 1 else "")
        for (relative, code), count in sorted(entries.items())
    ]


def main() -> None:
    current = source_hits()
    audited = audit_hits()
    missing = current - audited
    stale = audited - current
    if missing or stale:
        if missing:
            print("missing FreeBSD audit rows:", *show(missing), sep="\n  ", file=sys.stderr)
            print("\nadd a row per hit, e.g.:", file=sys.stderr)
            for relative, code in sorted(missing)[:3]:
                print(f"  | `{relative}` | {encode(code)} | ... |", file=sys.stderr)
        if stale:
            print("stale FreeBSD audit rows:", *show(stale), sep="\n  ", file=sys.stderr)
        raise SystemExit(1)
    print(f"FreeBSD platform audit covers all {sum(current.values())} source hits")


if __name__ == "__main__":
    main()
