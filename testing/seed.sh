#!/usr/bin/env bash
# Fill both servers with COUNT entries so we can test large-directory behaviour.
#
#   ./seed.sh          1000 entries   (fast, everyday development)
#   ./seed.sh 100000   the performance target
set -euo pipefail
cd "$(dirname "$0")"

COUNT="${1:-1000}"

echo "seeding SFTP with $COUNT entries..."
mkdir -p "data/sftp/big"
# One touch invocation per 5000 names rather than per file.
seq 1 "$COUNT" \
  | awk '{ printf "data/sftp/big/file-%06d.dat\n", $1 }' \
  | xargs -n 5000 touch
# A few entries with distinct sizes and dates, for sorting checks.
mkdir -p data/sftp/mixed
head -c 1048576 /dev/urandom > data/sftp/mixed/one-megabyte.bin
head -c 4096 /dev/urandom > data/sftp/mixed/four-kilobytes.bin
: > data/sftp/mixed/empty.txt
mkdir -p data/sftp/mixed/subdirectory

echo "seeding S3 with $COUNT objects..."
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/big"
seq 1 "$COUNT" \
  | awk -v d="$STAGE/big" '{ printf "%s/object-%06d.dat\n", d, $1 }' \
  | xargs -n 5000 touch

docker run --rm --network "$(docker compose ps -q minio >/dev/null 2>&1 && echo container:ferry-minio || echo host)" \
  -v "$STAGE:/stage:ro" --entrypoint sh minio/mc:latest -c "
    mc alias set local http://127.0.0.1:9000 ferryadmin ferryadmin >/dev/null &&
    mc cp --quiet --recursive /stage/big local/ferry-test/ >/dev/null &&
    echo done
  "

echo "seeded $COUNT entries into sftp:/upload/big and s3://ferry-test/big/"
