#!/usr/bin/env bash
# Deploy bacain_chatbot: pull latest main, rebuild the image, restart the
# container, and verify the bot actually connected before exiting.
#
# Safe to run from GitHub Actions over SSH (non-interactive) or by hand.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> pulling latest main"
git fetch origin main
git reset --hard origin/main

echo "==> checking .env present"
[ -f .env ] || { echo "FATAL: .env missing — refusing to deploy"; exit 1; }

echo "==> ensuring the SQLite volume is writable by the container user (uid 10001)"
# The rootfs is read-only and the bot runs as the unprivileged `linkbot` user;
# a FRESH named volume is created root-owned, so the very first start fails with
# "failed to open store at /data/linkbot.db: unable to open database file" and
# crash-loops (seen live 2026-09-23 on the first deploy that carried the SQLite
# store). Chown it from a throwaway root container before starting the bot.
VOLUME="$(docker compose config --format json 2>/dev/null \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(iter(d.get("volumes",{})), ""))' 2>/dev/null || true)"
if [ -n "$VOLUME" ]; then
  PROJECT="$(basename "$PWD")"
  FULL="${PROJECT}_${VOLUME}"
  docker volume create "$FULL" >/dev/null 2>&1 || true
  docker run --rm -v "$FULL":/data alpine sh -c 'chown 10001:10001 /data && chmod 755 /data' >/dev/null 2>&1 \
    && echo "    ok: $FULL" \
    || echo "    WARN: could not repair $FULL (continuing)"
fi

echo "==> rebuilding + restarting container"
docker compose up -d --build

echo "==> waiting for bot to connect"
for i in $(seq 1 12); do
  if docker compose logs --tail 100 linkbot 2>/dev/null | grep -q "bot ready"; then
    echo "==> OK: bot is live"
    docker compose ps
    exit 0
  fi
  sleep 5
done

echo "FATAL: bot did not report ready within 60s — deploy failed"
docker compose ps
docker compose logs --tail 50 linkbot
exit 1
