#!/usr/bin/env bash
# Stop the servers. Pass --wipe to also drop the MinIO volume and SFTP files.
set -euo pipefail
cd "$(dirname "$0")"

if [[ "${1:-}" == "--wipe" ]]; then
  docker compose down -v
  rm -rf data/sftp keys
  # Wiping regenerates the server identity, so drop the recorded one too.
  ssh-keygen -R "[127.0.0.1]:2222" >/dev/null 2>&1 || true
else
  docker compose down
fi
