#!/usr/bin/env python3
"""Check that a release's signing keys match the public keys the apps trust.

Two mistakes a release can make without any job failing: an updater private key that doesn't
belong to `NUGGET_UPDATER_PUBKEY` (installed apps then refuse every update), and a bundled model
manifest without a signature from `NUGGET_MANIFEST_RELEASE_PUBKEY` (the app then offers no model
downloads). This script catches both before anything is drafted.

  check_release_keys.py updater <file> <file.sig> <pubkey>   # tauri signer output vs public key
  check_release_keys.py manifest <default.json> <default.json.sig> <hex pubkey>

Ed25519 verification uses `openssl pkeyutl -rawin` (OpenSSL 3), so no Python packages are
needed. Exit status 0 means the signature verifies with the given public key.
"""

import base64
import hashlib
import os
import subprocess
import sys
import tempfile

# DER SubjectPublicKeyInfo prefix for a raw 32-byte Ed25519 key.
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def ed25519_verify(pub: bytes, msg: bytes, sig: bytes) -> bool:
    if len(pub) != 32 or len(sig) != 64:
        return False
    with tempfile.TemporaryDirectory() as d:
        paths = {n: os.path.join(d, n) for n in ("pub.der", "msg", "sig")}
        for name, data in (("pub.der", ED25519_SPKI_PREFIX + pub), ("msg", msg), ("sig", sig)):
            with open(paths[name], "wb") as f:
                f.write(data)
        r = subprocess.run(
            ["openssl", "pkeyutl", "-verify", "-pubin", "-inkey", paths["pub.der"],
             "-keyform", "DER", "-rawin", "-in", paths["msg"], "-sigfile", paths["sig"]],
            capture_output=True,
        )
        return r.returncode == 0


def minisign_lines(b64_text: str) -> list:
    """Tauri wraps minisign files (keys and signatures) in one more layer of base64."""
    text = base64.b64decode(b64_text.strip()).decode("utf-8")
    return [line for line in text.splitlines() if line.strip()]


def check_updater(path: str, sig_path: str, pubkey_b64: str) -> str:
    pub_lines = minisign_lines(pubkey_b64)
    pub = base64.b64decode(pub_lines[1])  # "Ed" + key id (8) + key (32)
    if len(pub) != 42 or pub[:2] != b"Ed":
        return "NUGGET_UPDATER_PUBKEY is not a minisign public key"
    with open(sig_path, encoding="utf-8") as f:
        sig_lines = minisign_lines(f.read())
    sig = base64.b64decode(sig_lines[1])  # algorithm (2) + key id (8) + signature (64)
    if len(sig) != 74 or sig[:2] not in (b"Ed", b"ED"):
        return f"{sig_path} is not a minisign signature"
    if sig[2:10] != pub[2:10]:
        return (f"{sig_path} was made with key id {sig[2:10][::-1].hex().upper()}, but "
                f"NUGGET_UPDATER_PUBKEY has key id {pub[2:10][::-1].hex().upper()}: "
                "TAURI_SIGNING_PRIVATE_KEY does not belong to NUGGET_UPDATER_PUBKEY")
    with open(path, "rb") as f:
        data = f.read()
    # "ED" is minisign's prehashed mode (BLAKE2b-512 of the file), which `tauri signer` uses.
    msg = hashlib.blake2b(data).digest() if sig[:2] == b"ED" else data
    if not ed25519_verify(pub[10:], msg, sig[10:]):
        return f"{sig_path} does not verify with NUGGET_UPDATER_PUBKEY"
    return ""


def check_manifest(path: str, sig_path: str, pubkey_hex: str) -> str:
    try:
        pub = bytes.fromhex(pubkey_hex.strip())
    except ValueError:
        return "NUGGET_MANIFEST_RELEASE_PUBKEY is not hex"
    with open(path, "rb") as f:
        data = f.read()
    with open(sig_path, encoding="utf-8") as f:
        lines = [l.strip() for l in f if l.strip() and not l.lstrip().startswith("#")]
    for line in lines:
        try:
            sig = bytes.fromhex(line)
        except ValueError:
            continue
        if ed25519_verify(pub, data, sig):
            return ""
    return (f"no signature in {sig_path} verifies with NUGGET_MANIFEST_RELEASE_PUBKEY: append a "
            "release signature (sign-manifest --append) before tagging, or the release offers no "
            "model downloads")


def main(argv: list) -> int:
    if len(argv) != 5 or argv[1] not in ("updater", "manifest"):
        print(__doc__, file=sys.stderr)
        return 2
    check = check_updater if argv[1] == "updater" else check_manifest
    err = check(argv[2], argv[3], argv[4])
    if err:
        print(f"error: {err}", file=sys.stderr)
        return 1
    print(f"{argv[1]}: {argv[3]} verifies with the configured public key")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
