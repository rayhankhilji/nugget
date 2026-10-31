#!/usr/bin/env python3
"""Release rehearsal, test only: serve a directory over HTTPS on localhost.

Serves ROOT (the rehearsal's update feed in ROOT/feed and the payloads in ROOT/dist) on
127.0.0.1:PORT and, when the host has IPv6, on [::1]:PORT too, with the throwaway localhost
certificate from make-ca.sh. Never listens on any other interface. Every request is appended
to --log (method, path, status, size), which the rehearsal keeps as evidence of what the app
fetched. Writes --ready-file once listening. Runs until killed.

Usage: python3 -I feed-server.py --root DIR --port N --cert server.crt --key server.key \
           --log access.log --ready-file ready
Standard library only.
"""

from __future__ import annotations

import argparse
import functools
import http.server
import socket
import ssl
import sys
import threading
import time
from pathlib import Path


class Handler(http.server.SimpleHTTPRequestHandler):
    log_path: Path

    def log_message(self, format: str, *args) -> None:  # noqa: A002 (stdlib signature)
        line = "%s %s %s\n" % (
            time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            self.address_string(),
            format % args,
        )
        with open(self.log_path, "a", encoding="utf-8") as f:
            f.write(line)
        sys.stderr.write(line)

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class Server6(Server):
    address_family = socket.AF_INET6

    def server_bind(self) -> None:
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        super().server_bind()


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--cert", type=Path, required=True)
    ap.add_argument("--key", type=Path, required=True)
    ap.add_argument("--log", type=Path, required=True)
    ap.add_argument("--ready-file", type=Path, required=True)
    args = ap.parse_args(argv)

    root = args.root.resolve(strict=True)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(args.cert, args.key)

    Handler.log_path = args.log
    handler = functools.partial(Handler, directory=str(root))
    servers = [Server(("127.0.0.1", args.port), handler)]
    try:
        servers.append(Server6(("::1", args.port), handler))
    except OSError as e:  # no IPv6 loopback: clients fall back to 127.0.0.1
        print(f"feed-server: not listening on [::1]: {e}", file=sys.stderr)
    for s in servers:
        s.socket = ctx.wrap_socket(s.socket, server_side=True)

    threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in servers]
    for t in threads:
        t.start()
    args.ready_file.write_text(f"{args.port}\n", encoding="utf-8")
    print(f"feed-server: serving {root} on https://localhost:{args.port}/", file=sys.stderr)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
