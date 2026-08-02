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
