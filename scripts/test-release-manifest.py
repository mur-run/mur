#!/usr/bin/env python3
"""Black-box tests for the canonical release manifest CLI."""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
HELPER = REPO_ROOT / "scripts" / "release-manifest.py"
VALID_MANIFEST = """\
schema = 1

[[binary]]
name = "mur"
package = "mur-core"

[[binary]]
name = "mur-mcp-server"
package = "mur-mcp-server"

[[binary]]
name = "murmurd"
package = "mur-daemon"

[[binary]]
name = "mur-agent-runtime"
package = "mur-agent-runtime"

[[binary]]
name = "mur-research-gateway"
package = "mur-research-gateway"
"""


class ReleaseManifestCliTests(unittest.TestCase):
    maxDiff = None

    def run_helper(
        self, *arguments: str, manifest: str = VALID_MANIFEST
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            (root / "scripts").mkdir()
            (root / "release").mkdir()
            shutil.copy2(HELPER, root / "scripts" / HELPER.name)
            (root / "release" / "binaries.toml").write_text(manifest, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(root / "scripts" / HELPER.name), *arguments],
                text=True,
                capture_output=True,
                check=False,
            )

    def test_names_preserve_packaging_order(self) -> None:
        result = self.run_helper("names")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            "mur mur-mcp-server murmurd mur-agent-runtime mur-research-gateway\n",
        )

    def test_cargo_packages_emits_one_pair_per_binary(self) -> None:
        result = self.run_helper("cargo-packages")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.split(),
            [
                "-p",
                "mur-core",
                "-p",
                "mur-mcp-server",
                "-p",
                "mur-daemon",
                "-p",
                "mur-agent-runtime",
                "-p",
                "mur-research-gateway",
            ],
        )

    def test_windows_paths_have_exe_suffix(self) -> None:
        result = self.run_helper("paths", "--windows")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.split(),
            [
                "mur.exe",
                "mur-mcp-server.exe",
                "murmurd.exe",
                "mur-agent-runtime.exe",
                "mur-research-gateway.exe",
            ],
        )

    def test_json_is_single_line_machine_readable_output(self) -> None:
        result = self.run_helper("json")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), 1)
        self.assertEqual(json.loads(result.stdout)[0], {"name": "mur", "package": "mur-core"})

    def test_duplicate_name_exits_two(self) -> None:
        manifest = VALID_MANIFEST + '\n[[binary]]\nname = "mur"\npackage = "other"\n'
        result = self.run_helper("names", manifest=manifest)
        self.assertEqual(result.returncode, 2)

    def test_duplicate_package_exits_two(self) -> None:
        manifest = VALID_MANIFEST + '\n[[binary]]\nname = "other"\npackage = "mur-core"\n'
        result = self.run_helper("names", manifest=manifest)
        self.assertEqual(result.returncode, 2)

    def test_unknown_schema_exits_two(self) -> None:
        result = self.run_helper("names", manifest=VALID_MANIFEST.replace("schema = 1", "schema = 2"))
        self.assertEqual(result.returncode, 2)

    def test_empty_manifest_exits_two(self) -> None:
        result = self.run_helper("names", manifest="schema = 1\n")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
