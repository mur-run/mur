#!/usr/bin/env python3
"""Read the repository's canonical shipped-binary manifest."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path


MANIFEST = Path(__file__).resolve().parents[1] / "release" / "binaries.toml"


def fail(message: str) -> "NoReturn":
    print(f"release manifest error: {message}", file=sys.stderr)
    raise SystemExit(2)


def load_binaries() -> list[dict[str, str]]:
    try:
        document = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(str(error))

    if document.get("schema") != 1:
        fail("schema must be 1")
    binaries = document.get("binary")
    if not isinstance(binaries, list) or not binaries:
        fail("binary list must not be empty")

    names: set[str] = set()
    packages: set[str] = set()
    validated: list[dict[str, str]] = []
    for index, binary in enumerate(binaries):
        if not isinstance(binary, dict) or set(binary) != {"name", "package"}:
            fail(f"binary {index} must contain only name and package")
        name = binary["name"]
        package = binary["package"]
        if not isinstance(name, str) or not name:
            fail(f"binary {index} has an invalid name")
        if not isinstance(package, str) or not package:
            fail(f"binary {index} has an invalid package")
        if name in names:
            fail(f"duplicate binary name: {name}")
        if package in packages:
            fail(f"duplicate Cargo package: {package}")
        names.add(name)
        packages.add(package)
        validated.append({"name": name, "package": package})
    return validated


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("names", "cargo-packages", "json", "paths"))
    parser.add_argument("--windows", action="store_true")
    return parser.parse_args()


def main() -> None:
    arguments = parse_args()
    binaries = load_binaries()
    if arguments.windows and arguments.command != "paths":
        fail("--windows is valid only with paths")

    if arguments.command == "names":
        output = " ".join(binary["name"] for binary in binaries)
    elif arguments.command == "cargo-packages":
        output = " ".join(
            token
            for binary in binaries
            for token in ("-p", binary["package"])
        )
    elif arguments.command == "json":
        output = json.dumps(binaries, separators=(",", ":"))
    else:
        suffix = ".exe" if arguments.windows else ""
        output = " ".join(binary["name"] + suffix for binary in binaries)
    print(output)


if __name__ == "__main__":
    main()
