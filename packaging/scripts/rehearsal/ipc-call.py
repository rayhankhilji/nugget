#!/usr/bin/env python3
"""Release rehearsal, test only: one JSON-RPC call to a running nuggetd over its Unix socket.

The daemon's IPC is JSON-RPC 2.0, one JSON object per line (crates/nugget-ipc/src/wire.rs);
the socket sits in a 0700 directory and the daemon checks the peer's uid, so only the same user
can call it. Prints the response object (as one JSON line) and exits 0 if the call returned a
result, 3 if it returned an error, 2 on a transport failure or timeout.

Usage: python3 -I ipc-call.py SOCKET METHOD [PARAMS_JSON] [--timeout SECONDS]
"""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("socket")
    ap.add_argument("method")
    ap.add_argument("params", nargs="?", default="null")
    ap.add_argument("--timeout", type=float, default=30.0)
    args = ap.parse_args(argv)

    request = {"jsonrpc": "2.0", "id": 1, "method": args.method}
    params = json.loads(args.params)
    if params is not None:
        request["params"] = params
    deadline = time.monotonic() + args.timeout
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(args.timeout)
            s.connect(args.socket)
            s.sendall((json.dumps(request) + "\n").encode())
            buf = b""
            while time.monotonic() < deadline:
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    if not line.strip():
                        continue
                    msg = json.loads(line)
                    if msg.get("id") != 1:
                        continue  # a notification
                    print(json.dumps(msg))
                    return 0 if "result" in msg else 3
    except (OSError, ValueError) as e:
        print(f"ipc-call: {e}", file=sys.stderr)
        return 2
    print("ipc-call: no response", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
