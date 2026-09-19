#!/usr/bin/env python3
"""Fail when the FreeBSD platform audit drifts from Rust source matches."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AUDIT = ROOT / "docs" / "platforms" / "freebsd-audit.md"
PATTERN = re.compile(r"target_os|target_family|cfg!\(|std::env::consts::OS|systemd|launchd|notify-send|osascript")
ROW = re.compile(r"^\| `([^`]+:\d+)` \|")


def source_hits() -> set[str]:
    result = subprocess.run(
        ["git", "ls-files", "*.rs"], cwd=ROOT, text=True, capture_output=True, check=True
    )
    hits: set[str] = set()
    for relative in result.stdout.splitlines():
        path = ROOT / relative
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if PATTERN.search(line):
                hits.add(f"{relative}:{number}")
    return hits


def audit_hits() -> set[str]:
    rows: list[str] = []
    for line in AUDIT.read_text(encoding="utf-8").splitlines():
        if match := ROW.match(line):
            rows.append(match.group(1))
    duplicates = sorted({row for row in rows if rows.count(row) > 1})
    if duplicates:
        print("duplicate audit rows:", *duplicates, sep="\n  ", file=sys.stderr)
        raise SystemExit(1)
    return set(rows)


def main() -> None:
    current = source_hits()
    audited = audit_hits()
    missing = sorted(current - audited)
    stale = sorted(audited - current)
    if missing or stale:
        if missing:
            print("missing FreeBSD audit rows:", *missing, sep="\n  ", file=sys.stderr)
        if stale:
            print("stale FreeBSD audit rows:", *stale, sep="\n  ", file=sys.stderr)
        raise SystemExit(1)
    print(f"FreeBSD platform audit covers all {len(current)} source hits")


if __name__ == "__main__":
    main()
