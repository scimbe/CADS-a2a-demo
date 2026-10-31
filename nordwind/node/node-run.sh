#!/usr/bin/env bash
# Nordwind node supervisor: keeps one `ct-agent channel` acceptor per served channel alive
# (outbound-only: no listener, nothing published). Works the same in a Linux container and
# natively on macOS. Config: an env file written by nordwind-provision.sh plus plane address.
#
#   NORDWIND_ENV_FILE=/path/zentrale.env ./node-run.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -n "${NORDWIND_ENV_FILE:-}" ] && { set -a; . "$NORDWIND_ENV_FILE"; set +a; }
: "${NORDWIND_NODE:?NORDWIND_NODE missing (env file from nordwind-provision.sh)}"
: "${NW_SERVE:=}"
export CT_AGENT_BIN="${CT_AGENT_BIN:-ct-agent}"
export CT_CHANNEL_BROKER="${CT_CHANNEL_BROKER:-bunsenbrenner.org:4435}"
export CT_CHANNEL_RELAY="${CT_CHANNEL_RELAY:-bunsenbrenner.org:4436}"
export CT_CHANNEL_FRONT_DOOR="${CT_CHANNEL_FRONT_DOOR:-bunsenbrenner.org:443}"
export NORDWIND_STATE_DIR="${NORDWIND_STATE_DIR:-${TMPDIR:-/tmp}/nordwind-$NORDWIND_NODE}"
mkdir -p "$NORDWIND_STATE_DIR"
[ "${NW_FRONT_DOOR_ONLY:-0}" = 1 ] && export CT_CHANNEL_FRONT_DOOR_ONLY=1

if [ -z "${CT_CHANNEL_FRONT_DOOR_CERT:-}" ]; then
  CP="${NW_CP_URL:-https://bunsenbrenner.org}"
  CT_CHANNEL_FRONT_DOOR_CERT="$(curl -fsS "${CP%/}/pki/ca" | xxd -p | tr -d '\n')"
  [ -n "$CT_CHANNEL_FRONT_DOOR_CERT" ] || { echo "node-run: empty /pki/ca from $CP" >&2; exit 1; }
  export CT_CHANNEL_FRONT_DOOR_CERT
fi

# Services offered per channel (least privilege: the phone channel cannot ask for mesh_status).
services_for() {
  case "$NORDWIND_NODE:$1" in
    zentrale:aussendienst-zentrale) echo "toern_briefing,ping" ;;
    zentrale:*) echo "toern_briefing,mesh_status,ping" ;;
    hafenbuero:*) echo "route_wetter,ping" ;;
    werkstatt:*) echo "draft_text,ping" ;;
    *) echo "ping" ;;
  esac
}

serve_channel() {
  local ch="$1" p grant; p="NW_CH_$(echo "$ch" | tr 'a-z-' 'A-Z_')_"
  grant="${p}GRANT"; grant="${!grant:-}"
  [ -n "$grant" ] || { echo "node-run: no grant for $ch (${p}GRANT)" >&2; return 1; }
  while true; do
    echo "node-run[$NORDWIND_NODE]: serving $ch ($(services_for "$ch"))"
    NORDWIND_SERVING_CHANNEL="$ch" \
    CT_CHANNEL_ROLE=accept CT_CHANNEL_SERVE=1 CT_CHANNEL_RELAY_ONLY=1 CT_CHANNEL_DIRECT_UPGRADE=1 \
    CT_CHANNEL_HOLDER_KEY="$NW_HOLDER_KEY" CT_CHANNEL_NOISE_KEY="$NW_NOISE_KEY" CT_CHANNEL_GRANT="$grant" \
    CT_AGENT_SERVICES="$(services_for "$ch")" \
    CT_AGENT_SERVICE_HANDLER_CMD="python3 $HERE/nordwind_node.py" \
      "$CT_AGENT_BIN" channel
    echo "node-run[$NORDWIND_NODE]: $ch exited ($?), restarting in 3s" >&2
    sleep 3
  done
}

pids=()
trap 'kill "${pids[@]}" 2>/dev/null; exit 0' TERM INT
IFS=, read -ra CHS <<<"$NW_SERVE"
[ "${#CHS[@]}" -gt 0 ] && [ -n "${CHS[0]}" ] || { echo "node-run: NW_SERVE empty - nothing to serve" >&2; exit 1; }
for ch in "${CHS[@]}"; do serve_channel "$ch" & pids+=($!); done
wait
