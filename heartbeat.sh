#!/usr/bin/env bash
# heartbeat.sh -- run this alongside your persistent `ct-agent channel --serve` process
# (see GitHub CADS-Tunnel#248 for your grant/env block). POSTs your shared token every
# ~15s so the a2a-demo dashboard (https://a2a-demo.bunsenbrenner.org/) knows you're
# online and shows/hides the scenarios you're part of accordingly -- this is a simple
# "are you still there" signal, deliberately separate from the A2A channel itself (the
# operator's own choice: it should keep working even if a channel dial is mid-retry).
#
# Usage:
#   HEARTBEAT_PEER=bob1 HEARTBEAT_TOKEN=<yours, from #248> ./heartbeat.sh
#   HEARTBEAT_PEER=bob2 HEARTBEAT_TOKEN=<yours, from #248> ./heartbeat.sh
#
# Runs in the foreground; background it yourself (nohup/&, a systemd unit, tmux -- your
# call, matching this project's other scripts).
set -euo pipefail

PEER="${HEARTBEAT_PEER:?set HEARTBEAT_PEER=bob1 or bob2}"
TOKEN="${HEARTBEAT_TOKEN:?set HEARTBEAT_TOKEN=<your shared secret from CADS-Tunnel#248>}"
URL="${HEARTBEAT_URL:-https://a2a-demo.bunsenbrenner.org/heartbeat/${PEER}}"
INTERVAL="${HEARTBEAT_INTERVAL_SECS:-15}"

echo "heartbeat.sh: POSTing to $URL every ${INTERVAL}s as $PEER" >&2
while true; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST --data "$TOKEN" "$URL" --max-time 10 || echo "000")
  case "$code" in
    204) : ;; # normal, no output
    401) echo "heartbeat.sh: 401 unauthorized -- check HEARTBEAT_TOKEN" >&2 ;;
    404) echo "heartbeat.sh: 404 -- check HEARTBEAT_PEER (must be bob1 or bob2)" >&2 ;;
    *) echo "heartbeat.sh: unexpected response $code, retrying next interval" >&2 ;;
  esac
  sleep "$INTERVAL"
done
