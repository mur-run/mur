#!/usr/bin/env python3
"""Two loopback "shops" for the browser live-mode acceptance tests (F1-F7).

Each shop is an HTTPS server on 127.0.0.1 at an ephemeral port with one
product page carrying a known price. HTTPS is not optional: the runtime's
egress proxy forwards `CONNECT` only (Gap 3), so a plain-HTTP fixture would
get `501 Not Implemented` before the test learned anything.

The certificate is self-signed and minted on every start with `openssl`,
so nothing secret is checked in. Browsers must be told to accept it
(`@playwright/mcp --ignore-https-errors`).

Protocol (so a Rust test or a shell script can drive it blind):

    stdout, first line, then flushed:
        {"cheaper":"magpie","shops":{"starling":{"port":N,"price":"NT$420",
         "name":"Starling Stationery","url":"https://127.0.0.1:N/"}, ...}}
    stderr, one line per request:
        <shop> <method> <path> host=<Host header>
    exit: on SIGTERM / SIGINT, or when --parent-pid stops existing.

Usage:
    browser-live-fixture.py [--parent-pid PID]
"""

import argparse
import json
import os
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The shops. Prices are the only facts F1 asserts on, so keep them distinct
# from anything else a page or a log line might contain.
SHOPS = {
    "starling": {
        "name": "Starling Stationery",
        "product": "Blue Feather Fountain Pen",
        "price": "NT$420",
    },
    "magpie": {
        "name": "Magpie Market",
        "product": "Blue Feather Fountain Pen",
        "price": "NT$385",
    },
}
CHEAPER = "magpie"

PAGE = """<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>{name} - {product}</title></head>
<body>
  <header><h1>{name}</h1></header>
  <main>
    <article>
      <h2 id="product">{product}</h2>
      <p>Price: <strong id="price">{price}</strong></p>
      <p>In stock. Ships from Taipei.</p>
    </article>
  </main>
</body>
</html>
"""


def mint_cert(cert_dir: str) -> tuple[str, str]:
    """Self-signed cert for 127.0.0.1 (and localhost, so the F2 denied host
    fails at the proxy rather than at TLS)."""
    key = os.path.join(cert_dir, "key.pem")
    cert = os.path.join(cert_dir, "cert.pem")
    subprocess.run(
        [
            "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", key, "-out", cert, "-days", "2",
            "-subj", "/CN=127.0.0.1",
            "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost",
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.chmod(key, 0o600)
    return cert, key


def make_handler(shop_id: str):
    shop = SHOPS[shop_id]
    body = PAGE.format(**shop).encode()

    class Handler(BaseHTTPRequestHandler):
        server_version = "mur-fixture/1"

        def log_message(self, fmt, *args):  # noqa: N802 (stdlib name)
            host = self.headers.get("Host", "-")
            sys.stderr.write(f"{shop_id} {self.command} {self.path} host={host}\n")
            sys.stderr.flush()

        def do_GET(self):  # noqa: N802
            if self.path in ("/", "/index.html"):
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            else:
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()

    return Handler


def serve(shop_id: str, ctx: ssl.SSLContext) -> ThreadingHTTPServer:
    srv = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(shop_id))
    srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--parent-pid", type=int, default=None,
                    help="exit when this pid is gone (for test harnesses)")
    args = ap.parse_args()

    with tempfile.TemporaryDirectory(prefix="mur-fixture-") as cert_dir:
        cert, key = mint_cert(cert_dir)
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert, key)

        servers = {sid: serve(sid, ctx) for sid in SHOPS}
        announce = {
            "cheaper": CHEAPER,
            "shops": {
                sid: {
                    "name": SHOPS[sid]["name"],
                    "price": SHOPS[sid]["price"],
                    "port": srv.server_address[1],
                    "url": f"https://127.0.0.1:{srv.server_address[1]}/",
                }
                for sid, srv in servers.items()
            },
        }
        sys.stdout.write(json.dumps(announce) + "\n")
        sys.stdout.flush()

        stop = threading.Event()
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, lambda *_: stop.set())
        while not stop.is_set():
            if args.parent_pid is not None:
                try:
                    os.kill(args.parent_pid, 0)
                except OSError:
                    break
            time.sleep(0.2)
        for srv in servers.values():
            srv.shutdown()
    return 0


if __name__ == "__main__":
    sys.exit(main())
