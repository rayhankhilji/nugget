#!/usr/bin/env bash
# Release rehearsal, test only: make a throwaway local CA, name-constrained to the loopback names,
# and a server certificate for localhost (DNS:localhost, IP:127.0.0.1, IP:::1), both valid for two
# days, so the rehearsal can serve its update feed over HTTPS. Release builds of the app refuse plain-HTTP update endpoints and verify TLS
# against the OS trust store (rustls-platform-verifier), so the CA is added to the runner's
# store for the duration of the job (trust-ca.sh, or the Windows step) and removed afterwards.
# The CA's private key is deleted as soon as the server certificate is signed.
#
# Usage: make-ca.sh OUT_DIR   ->  OUT_DIR/{ca.crt, server.crt, server.key}
set -euo pipefail

out="${1:?usage: make-ca.sh OUT_DIR}"
mkdir -p "$out"
cd "$out"
umask 077

# MSYS (Git Bash) would turn "/CN=..." into a Windows path.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

cat >ca.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = Nugget release rehearsal CA (throwaway, test only)
[ext]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
# Even if its key survived, this CA could vouch for nothing but the loopback names.
nameConstraints = critical, permitted;DNS:localhost, permitted;IP:127.0.0.1/255.255.255.255, permitted;IP:0:0:0:0:0:0:0:1/FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF:FFFF
subjectKeyIdentifier = hash
EOF
cat >server.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
[ext]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, IP:127.0.0.1, IP:::1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
EOF

openssl ecparam -name prime256v1 -genkey -noout -out ca.key
openssl req -new -x509 -sha256 -days 2 -key ca.key -config ca.cnf -extensions ext -out ca.crt
openssl ecparam -name prime256v1 -genkey -noout -out server.key
openssl req -new -sha256 -key server.key -config server.cnf -out server.csr
openssl x509 -req -sha256 -days 2 -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -extfile server.cnf -extensions ext -out server.crt
# Nothing else may ever be signed by this CA.
rm -f ca.key ca.srl server.csr ca.cnf server.cnf
openssl verify -CAfile ca.crt server.crt
chmod 644 ca.crt server.crt
echo "CA:     $(openssl x509 -in ca.crt -noout -subject -fingerprint -sha256)"
echo "server: $(openssl x509 -in server.crt -noout -subject -ext subjectAltName 2>/dev/null | tr '\n' ' ')"
