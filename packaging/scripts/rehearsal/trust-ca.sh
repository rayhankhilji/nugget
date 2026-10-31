#!/usr/bin/env bash
# Release rehearsal, TEST ONLY (Linux CI runners): add the throwaway rehearsal CA from
# make-ca.sh to the system trust store, or remove it again. The app verifies TLS against the OS
# store (rustls-platform-verifier), so its HTTPS localhost feed needs this. The CA's private key
# was deleted right after it signed the one localhost certificate, so trusting it lets nothing
# else through. The workflow removes it in an always() step; the runner is discarded anyway.
#
# Usage: trust-ca.sh add CA.crt | trust-ca.sh remove
set -euo pipefail

dest=/usr/local/share/ca-certificates/nugget-release-rehearsal-ca.crt
case "${1:-}" in
  add)
    sudo install -m 644 "${2:?usage: trust-ca.sh add CA.crt}" "$dest"
    sudo update-ca-certificates
    grep -qF "$(sed -n 2p "$2")" /etc/ssl/certs/ca-certificates.crt ||
      { echo "trust-ca: the CA is not in /etc/ssl/certs/ca-certificates.crt" >&2; exit 1; }
    ;;
  remove)
    if [ -e "$dest" ]; then
      sudo rm -f "$dest"
      sudo update-ca-certificates --fresh >/dev/null
      echo "trust-ca: removed the rehearsal CA"
    fi
    ;;
  *) echo "usage: trust-ca.sh add CA.crt | trust-ca.sh remove" >&2; exit 2 ;;
esac
