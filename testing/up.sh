#!/usr/bin/env bash
# Start the local SFTP and S3 servers. Idempotent.
set -euo pipefail
cd "$(dirname "$0")"

mkdir -p data/sftp keys/host

if [[ ! -f keys/id_ed25519 ]]; then
  echo "generating throwaway keypair -> keys/id_ed25519"
  ssh-keygen -t ed25519 -N "" -C "ferry-test" -f keys/id_ed25519 >/dev/null
fi

# The server's own identity, generated once and reused, so recreating the
# container does not look like the host key changing under you.
if [[ ! -f keys/host/ssh_host_ed25519_key ]]; then
  echo "generating server host keys -> keys/host/"
  ssh-keygen -t ed25519 -N "" -C "ferry-test-host" -f keys/host/ssh_host_ed25519_key >/dev/null
  ssh-keygen -t rsa -b 3072 -N "" -C "ferry-test-host" -f keys/host/ssh_host_rsa_key >/dev/null
fi

docker compose up -d
docker compose wait minio-init >/dev/null 2>&1 || true

cat <<'EOF'

Ready.

  SFTP   sftp://ferry@127.0.0.1:2222   password: ferry
         key auth:  testing/keys/id_ed25519
         agent:     ssh-add testing/keys/id_ed25519

  S3     endpoint http://127.0.0.1:9000   bucket: ferry-test   region: us-east-1
         access key: ferryadmin   secret: ferryadmin
         console: http://127.0.0.1:9001

  Seed data:  ./seed.sh [count]     (default 1000)
  Stop:       ./down.sh
EOF
