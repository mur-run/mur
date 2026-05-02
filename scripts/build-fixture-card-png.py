#!/usr/bin/env python3
"""Generate silly-v3.png + silly-v2.png test fixtures for M4.3 tests.

Each PNG is a 1x1 transparent image with a tEXt chunk carrying base64-encoded
character JSON. V3 uses keyword `ccv3`; V2 uses `chara`.

Usage:
    python3 scripts/build-fixture-card-png.py
"""
import struct
import zlib
import base64
import json
import pathlib


def chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


def png_with_text_chunk(keyword: str, payload: str) -> bytes:
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr_data = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    ihdr = chunk(b"IHDR", ihdr_data)
    text = chunk(b"tEXt", keyword.encode() + b"\0" + payload.encode("latin-1"))
    raw = b"\x00\x00\x00\x00\x00"  # 1x1 transparent RGBA
    idat_data = zlib.compress(raw)
    idat = chunk(b"IDAT", idat_data)
    iend = chunk(b"IEND", b"")
    return sig + ihdr + text + idat + iend


def main() -> None:
    v3_card = {
        "spec": "chara_card_v3",
        "spec_version": "3.0",
        "data": {
            "name": "TestV3",
            "description": "imported via fixture",
            "character_book": {"name": "x", "entries": []},
        },
    }
    v3_payload = base64.b64encode(json.dumps(v3_card).encode()).decode()

    v2_card = {"name": "TestV2", "description": "v2 imported"}
    v2_payload = base64.b64encode(json.dumps(v2_card).encode()).decode()

    out = pathlib.Path("mur-core/tests/fixtures/cards")
    out.mkdir(parents=True, exist_ok=True)
    (out / "silly-v3.png").write_bytes(png_with_text_chunk("ccv3", v3_payload))
    (out / "silly-v2.png").write_bytes(png_with_text_chunk("chara", v2_payload))
    print(f"Wrote {(out / 'silly-v3.png').stat().st_size} bytes to silly-v3.png")
    print(f"Wrote {(out / 'silly-v2.png').stat().st_size} bytes to silly-v2.png")


if __name__ == "__main__":
    main()
