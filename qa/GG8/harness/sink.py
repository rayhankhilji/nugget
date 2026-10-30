#!/usr/bin/env python3
"""GG8 sink: the whole "internet" as the daemon's network namespace sees it.

* A DNS server that logs every query. In `sinkhole` mode every A query is answered with the sink
  address (203.0.113.10, classified `cloud` by Nugget) and AAAA with no records; in `internet` mode
  queries are forwarded to a real resolver and the answers logged.
* An HTTPS server on :443 (and plain HTTP on :80) that terminates TLS for any name (a certificate
  is minted on the fly from the GG8 test CA for the SNI it is asked for), records every request in
  full (headers and body) and answers it from a mock backend chosen by the Host header:
  - api.anthropic.com           Anthropic Messages API (streaming and not)
  - api.openai.com              OpenAI Responses API, chat completions, embeddings
  - api.cloudllm.test           any OpenAI-compatible cloud (chat completions, embeddings)
  - slack.com                   Slack Web API (see mocks.py)
  - everything else             404, recorded
* The same OpenAI-compatible mock on 192.168.88.1:8080 (a "LAN" model server).

Every request goes to <out>/requests.jsonl, every DNS query to <out>/dns.jsonl. Standard library
only, plus the `openssl` command for minting certificates.
"""
import argparse
import base64
import json
import os
import socket
import socketserver
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mocks  # noqa: E402

SINK_IP = "203.0.113.10"
LAN_IP = "192.168.88.1"
LOCK = threading.Lock()
OUT = None
MODE = "sinkhole"
UPSTREAM_DNS = "8.8.8.8"


def log(name, rec):
    rec["t"] = time.time()
    with LOCK:
        with open(os.path.join(OUT, name), "a") as f:
            f.write(json.dumps(rec) + "\n")


# ---------------------------------------------------------------------------------------------
# DNS


def parse_qname(data, off):
    labels = []
    while True:
        n = data[off]
        if n == 0:
            off += 1
            break
        if n & 0xC0:
            off += 2
            break
        labels.append(data[off + 1:off + 1 + n].decode("ascii", "replace"))
        off += 1 + n
    return ".".join(labels), off


QTYPES = {1: "A", 28: "AAAA", 5: "CNAME", 65: "HTTPS", 33: "SRV", 16: "TXT", 12: "PTR"}


def dns_answer(query):
    tid = query[:2]
    qname, off = parse_qname(query, 12)
    qtype, qclass = struct.unpack(">HH", query[off:off + 4])
    question = query[12:off + 4]
    flags = b"\x81\x80"
    if qtype == 1:
        ans = b"\xc0\x0c" + struct.pack(">HHIH", 1, 1, 30, 4) + socket.inet_aton(SINK_IP)
        return tid + flags + struct.pack(">HHHH", 1, 1, 0, 0) + question + ans, qname, qtype
    return tid + flags + struct.pack(">HHHH", 1, 0, 0, 0) + question, qname, qtype


def dns_server(bind):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((bind, 53))
    while True:
        data, addr = s.recvfrom(4096)
        try:
            if MODE == "sinkhole":
                resp, qname, qtype = dns_answer(data)
                answers = [SINK_IP] if qtype == 1 else []
            else:
                qname, off = parse_qname(data, 12)
                qtype = struct.unpack(">H", data[off:off + 2])[0]
                u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                u.settimeout(5)
                u.sendto(data, (UPSTREAM_DNS, 53))
                resp, _ = u.recvfrom(4096)
                u.close()
                answers = []
                if len(resp) > 12 and struct.unpack(">H", resp[6:8])[0] > 0:
                    # crude A extraction: 4-byte rdata after type 1 class 1
                    i = 0
                    while True:
                        i = resp.find(b"\x00\x01\x00\x01", i + 1)
                        if i < 0 or i + 10 + 4 > len(resp):
                            break
                        rdlen = struct.unpack(">H", resp[i + 8:i + 10])[0]
                        if rdlen == 4 and i > off:
                            answers.append(socket.inet_ntoa(resp[i + 10:i + 14]))
            log("dns.jsonl", {"from": addr[0], "qname": qname,
                              "qtype": QTYPES.get(qtype, qtype), "answers": answers})
            s.sendto(resp, addr)
        except Exception as e:  # noqa: BLE001
            log("dns.jsonl", {"error": repr(e)})


# ---------------------------------------------------------------------------------------------
# Certificates


class CertMint:
    def __init__(self, ca_dir):
        self.ca_dir = ca_dir
        self.ca_key = os.path.join(ca_dir, "ca.key")
        self.ca_crt = os.path.join(ca_dir, "ca.crt")
        self.cache = {}
        self.lock = threading.Lock()
        os.makedirs(os.path.join(ca_dir, "leaf"), exist_ok=True)

    def context_for(self, name):
        with self.lock:
            if name in self.cache:
                return self.cache[name]
            safe = "".join(c if c.isalnum() or c in ".-" else "_" for c in name)[:120]
            key = os.path.join(self.ca_dir, "leaf", safe + ".key")
            crt = os.path.join(self.ca_dir, "leaf", safe + ".crt")
            if not os.path.exists(crt):
                ext = tempfile.NamedTemporaryFile("w", delete=False, suffix=".ext")
                san = f"IP:{name}" if all(p.isdigit() for p in name.split(".")) else f"DNS:{name}"
                ext.write(f"subjectAltName={san}\nextendedKeyUsage=serverAuth\n"
                          "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\n")
                ext.close()
                csr = crt + ".csr"
                subprocess.run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
                                "-keyout", key, "-out", csr, "-subj", f"/CN={name}"],
                               check=True, capture_output=True)
                subprocess.run(["openssl", "x509", "-req", "-in", csr, "-CA", self.ca_crt,
                                "-CAkey", self.ca_key, "-CAcreateserial", "-out", crt,
                                "-days", "30", "-sha256", "-extfile", ext.name],
                               check=True, capture_output=True)
                os.unlink(ext.name)
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.set_alpn_protocols(["http/1.1"])
            ctx.load_cert_chain(crt, key)
            self.cache[name] = ctx
            return ctx


# ---------------------------------------------------------------------------------------------
# HTTP


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "gg8-sink"

    def log_message(self, *a):
        pass

    def read_body(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            out = b""
            while True:
                line = self.rfile.readline().strip()
                n = int(line.split(b";")[0], 16)
                if n == 0:
                    self.rfile.readline()
                    break
                out += self.rfile.read(n)
                self.rfile.readline()
            return out
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def handle_any(self):
        body = self.read_body()
        host = (self.headers.get("Host") or "").split(":")[0]
        sni = getattr(self.connection, "gg8_sni", None) if isinstance(
            self.connection, ssl.SSLSocket) else None
        try:
            text = body.decode("utf-8")
            body_rec = {"body": text}
        except UnicodeDecodeError:
            body_rec = {"body_b64": base64.b64encode(body).decode()}
        rec = {"scheme": "https" if isinstance(self.connection, ssl.SSLSocket) else "http",
               "listen": self.server.server_address[0] + ":" + str(self.server.server_address[1]),
               "client": self.client_address[0], "sni": sni, "host": host,
               "method": self.command, "path": self.path,
               "headers": [[k, v] for k, v in self.headers.items()], **body_rec,
               "bytes_in": len(body)}
        try:
            status, headers, payload = mocks.respond(host, self.command, self.path,
                                                     dict(self.headers.items()), body, self.server)
        except Exception as e:  # noqa: BLE001
            status, headers, payload = 500, {"Content-Type": "text/plain"}, repr(e).encode()
        rec["status"] = status
        if isinstance(payload, (bytes, bytearray)):
            rec["bytes_out"] = len(payload)
        log("requests.jsonl", rec)
        self.send_response(status)
        if callable(payload) or hasattr(payload, "__next__"):
            # A generator of chunks: server-sent events.
            for k, v in headers.items():
                self.send_header(k, v)
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for chunk in payload:
                if isinstance(chunk, str):
                    chunk = chunk.encode()
                self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        else:
            for k, v in headers.items():
                self.send_header(k, v)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = do_HEAD = do_OPTIONS = handle_any
    do_PROPFIND = do_REPORT = handle_any


class TLSServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, addr, mint):
        super().__init__(addr, Handler)
        self.mint = mint
        base = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        base.set_alpn_protocols(["http/1.1"])
        base.load_cert_chain(*self._default())
        holder = {}

        def sni_cb(sslobj, name, _ctx):
            name = name or SINK_IP
            holder[id(sslobj)] = name
            try:
                sslobj.context = mint.context_for(name)
            except Exception as e:  # noqa: BLE001
                log("requests.jsonl", {"tls_error": repr(e), "sni": name})
            return None

        base.sni_callback = sni_cb
        self.ctx = base
        self.sni_holder = holder

    def _default(self):
        ctx = self.mint.context_for(SINK_IP)  # noqa: F841 (mints the files)
        safe = SINK_IP
        return (os.path.join(self.mint.ca_dir, "leaf", safe + ".crt"),
                os.path.join(self.mint.ca_dir, "leaf", safe + ".key"))

    def get_request(self):
        sock, addr = self.socket.accept()
        sock.settimeout(30)
        try:
            tls = self.ctx.wrap_socket(sock, server_side=True)
        except Exception as e:  # noqa: BLE001
            log("requests.jsonl", {"tls_handshake_error": repr(e), "client": addr[0],
                                   "listen": f"{self.server_address[0]}:{self.server_address[1]}"})
            sock.close()
            raise OSError("handshake failed") from e
        tls.gg8_sni = self.sni_holder.pop(id(tls), None)
        return tls, addr


class TCPLogger(socketserver.ThreadingTCPServer):
    """Accepts and logs connections on ports nothing else serves (no payload answered)."""
    daemon_threads = True
    allow_reuse_address = True


class RawHandler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(3)
        try:
            first = self.request.recv(2048)
        except Exception:  # noqa: BLE001
            first = b""
        log("requests.jsonl", {"raw_tcp": True, "client": self.client_address[0],
                               "listen": f"{self.server.server_address[0]}:{self.server.server_address[1]}",
                               "first_bytes_b64": base64.b64encode(first).decode()})


def main():
    global OUT, MODE, UPSTREAM_DNS
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--ca-dir", required=True)
    ap.add_argument("--mode", default="sinkhole", choices=["sinkhole", "internet"])
    ap.add_argument("--upstream-dns", default="8.8.8.8")
    ap.add_argument("--extra-ports", default="", help="comma list of ports to log raw TCP on")
    a = ap.parse_args()
    OUT, MODE, UPSTREAM_DNS = a.out, a.mode, a.upstream_dns
    os.makedirs(OUT, exist_ok=True)
    mint = CertMint(a.ca_dir)
    threading.Thread(target=dns_server, args=(SINK_IP,), daemon=True).start()
    servers = []
    if MODE == "sinkhole":
        servers.append(TLSServer((SINK_IP, 443), mint))
        servers.append(ThreadingHTTPServer((SINK_IP, 80), Handler))
        for p in [int(x) for x in a.extra_ports.split(",") if x]:
            servers.append(TCPLogger((SINK_IP, p), RawHandler))
    lan = ThreadingHTTPServer((LAN_IP, 8080), Handler)
    servers.append(lan)
    for s in servers:
        s.daemon_threads = True
        threading.Thread(target=s.serve_forever, daemon=True).start()
    print("sink ready", flush=True)
    with open(os.path.join(OUT, "sink.ready"), "w") as f:
        f.write(str(os.getpid()))
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
