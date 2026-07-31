#!/bin/sh
# agent-alice's real startup: fetch the real Mesh-Plane CA root from the control
# plane's own published GET /pki/ca (the same trust anchor
# crates/edge/src/pki.rs::build_channel_front_door_acceptor issues the :443 channel
# front-door's leaf cert from -- confirmed by reading both sides directly), hex-encode
# it, and export CT_CHANNEL_FRONT_DOOR_CERT so the real relay-leg ladder
# (QUIC-to-relay-port -> TCP-over-:443-front-door) is actually wired up instead of only
# ever trying the QUIC rung. Verified live: forcing the QUIC relay port unreachable
# while this is set still delivers the message, via the real :443 handover.
set -eu
CP_URL="${CT_AGENT_CP_URL:?CT_AGENT_CP_URL required to fetch the Mesh-Plane CA root}"
CA_HEX=$(curl -fsS "$CP_URL/pki/ca" | xxd -p | tr -d '\n')
[ -n "$CA_HEX" ] || { echo "alice-entrypoint: empty /pki/ca response from $CP_URL, refusing to start without a front-door trust anchor" >&2; exit 1; }
export CT_CHANNEL_FRONT_DOOR_CERT="$CA_HEX"
exec ct-agent channel
