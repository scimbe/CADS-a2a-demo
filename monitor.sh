#!/usr/bin/env bash
# monitor.sh -- ship your `ct-agent channel --serve` process's own real log lines onto
# the a2a-demo dashboard's live event stream (https://a2a-demo.bunsenbrenner.org/), so
# whoever's debugging a round with you can see both sides without you pasting logs by
# hand. Deliberately separate from heartbeat.sh (which only ever says "still online") --
# reuses the SAME shared token, just as a header instead of the body.
#
# Usage: pipe your process's combined output straight into this script.
#
#   ct-agent channel 2>&1 | HEARTBEAT_PEER=bob1 HEARTBEAT_TOKEN=<yours, from #248> ./monitor.sh
#
# Run it alongside heartbeat.sh (both read the same env vars) -- two small foreground
# processes, background them yourself (nohup/&, a systemd unit, tmux, your call, same as
# every other script here). Safe to leave running continuously: idle stdin just means no
# POSTs go out, nothing to stop or restart when a round isn't in flight.
set -euo pipefail

PEER="${HEARTBEAT_PEER:?set HEARTBEAT_PEER=bob1 or bob2}"
TOKEN="${HEARTBEAT_TOKEN:?set HEARTBEAT_TOKEN=<your shared secret from CADS-Tunnel#248>}"
URL="${MONITOR_URL:-https://a2a-demo.bunsenbrenner.org/monitor/${PEER}}"

echo "monitor.sh: shipping stdin lines to $URL as $PEER" >&2
while IFS= read -r line; do
  [ -n "$line" ] || continue
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H "x-heartbeat-token: $TOKEN" \
    --data-binary "$line" \
    "$URL" --max-time 10 || echo "000")
  case "$code" in
    204) : ;; # shipped, no output
    401) echo "monitor.sh: 401 unauthorized -- check HEARTBEAT_TOKEN" >&2 ;;
    404) echo "monitor.sh: 404 -- check HEARTBEAT_PEER (must be bob1 or bob2)" >&2 ;;
    413) : ;; # one oversized line dropped -- not fatal, keep shipping the rest
    *) echo "monitor.sh: unexpected response $code for one line, continuing" >&2 ;;
  esac
done
