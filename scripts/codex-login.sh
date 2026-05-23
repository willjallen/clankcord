#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "Stopping Clankcord runtime..."
docker compose stop clankcord

echo "Starting one-shot Codex device login against ./clankcord/runtime-data/codex-home..."
docker compose run --rm --no-deps --entrypoint /bin/bash clankcord -lc '
  set -euo pipefail
  export CODEX_HOME=/codex
  codex logout
  codex login --device-auth
  codex login status
'

echo "Recreating Clankcord runtime with preserved /codex/auth.json..."
docker compose up -d --force-recreate clankcord

echo "Codex login complete."
