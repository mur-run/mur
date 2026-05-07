"""Validate hostile corpus structure (run as part of CI lint)."""
import json
import pathlib
import sys

try:
    import yaml
except ImportError:
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "pyyaml", "-q"])
    import yaml

_CARDS_DIR = pathlib.Path(__file__).parent / "cards"
_MCP_DIR = pathlib.Path(__file__).parent / "mcp"


def main() -> None:
    errors = []

    cards = sorted(_CARDS_DIR.glob("hostile_*.murcard.yaml"))
    if len(cards) < 20:
        errors.append(f"Expected >= 20 card files, got {len(cards)}")
    for c in cards:
        try:
            data = yaml.safe_load(c.read_text())
            assert "data" in data, f"{c.name}: missing 'data' key"
            assert "name" in data["data"], f"{c.name}: missing 'data.name'"
        except Exception as e:
            errors.append(f"{c.name}: {e}")

    mcps = sorted(_MCP_DIR.glob("hostile_*_manifest.json"))
    if len(mcps) < 10:
        errors.append(f"Expected >= 10 MCP manifest files, got {len(mcps)}")
    for m in mcps:
        try:
            data = json.loads(m.read_text())
            assert "name" in data, f"{m.name}: missing 'name'"
            assert "tools" in data, f"{m.name}: missing 'tools'"
        except Exception as e:
            errors.append(f"{m.name}: {e}")

    if errors:
        print("FAIL:")
        for e in errors:
            print(f"  {e}")
        sys.exit(1)
    print(f"OK: {len(cards)} cards + {len(mcps)} MCP manifests validated")


if __name__ == "__main__":
    main()
