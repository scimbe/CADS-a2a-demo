#!/usr/bin/env bash
# One-time (or re-run-to-rotate) real channel provisioning for agent-alice and
# agent-bob: mints two real Agent-Fabric identities, derives their real channel id,
# registers the channel + both members with the control plane over a real OIDC bearer
# token (POST /me/channels, POST /me/channels/:channel/members), and mints two real
# operator-signed grants. Writes every value `compose.a2a-demo.yml` needs into .env.
#
# This is the actual, once-ahead-of-time step described in the README's "What's real"
# section -- run it again to rotate identities, but a normal demo round never calls it;
# `run_round` in src/main.rs only ever dials with the fixed identity this script wrote.
#
# Requires: a real ct-agent binary (CT_AGENT_BIN, default `ct-agent` on PATH), curl,
# python3 (JSON parsing only -- no dependency beyond the stdlib), and a real account on
# the target CADS-Tunnel plane with permission to register a channel (any OIDC login
# works; the channel's `owner` becomes that account's subject).
set -euo pipefail

CT_AGENT_BIN="${CT_AGENT_BIN:-ct-agent}"
CP_URL="${A2A_CP_URL:?set A2A_CP_URL=https://<your plane>, e.g. https://bunsenbrenner.org}"
OIDC_ISSUER="${A2A_OIDC_ISSUER:?set A2A_OIDC_ISSUER=https://<auth host>/realms/<realm>}"
OPERATOR_EMAIL="${A2A_OPERATOR_EMAIL:?set A2A_OPERATOR_EMAIL=<account that will own this channel>}"
OPERATOR_PASSWORD="${A2A_OPERATOR_PASSWORD:?set A2A_OPERATOR_PASSWORD=<its password>}"
ENV_FILE="${A2A_ENV_FILE:-.env}"

command -v "$CT_AGENT_BIN" >/dev/null || { echo "provision.sh: ct-agent binary not found ($CT_AGENT_BIN) -- build/install it first" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "== minting operator identity =="
"$CT_AGENT_BIN" channel operator-init >"$work/operator.out"
operator_pub=$(grep -o 'operator_pubkey = [0-9a-f]*' "$work/operator.out" | awk '{print $3}')
operator_key=$(grep 'CT_CHANNEL_OPERATOR_KEY=' "$work/operator.out" | cut -d= -f2)

echo "== minting agent-alice (accept/serve) identity =="
"$CT_AGENT_BIN" channel init >"$work/alice.out"
alice_holder_pub=$(grep -o 'holder_pubkey = [0-9a-f]*' "$work/alice.out" | awk '{print $3}')
alice_noise_pub=$(grep -o 'noise_pubkey  = [0-9a-f]*' "$work/alice.out" | awk '{print $3}')
alice_holder_key=$(grep 'CT_CHANNEL_HOLDER_KEY=' "$work/alice.out" | cut -d= -f2)
alice_noise_key=$(grep 'CT_CHANNEL_NOISE_KEY=' "$work/alice.out" | cut -d= -f2)

echo "== minting agent-bob (initiate) identity =="
"$CT_AGENT_BIN" channel init >"$work/bob.out"
bob_holder_pub=$(grep -o 'holder_pubkey = [0-9a-f]*' "$work/bob.out" | awk '{print $3}')
bob_noise_pub=$(grep -o 'noise_pubkey  = [0-9a-f]*' "$work/bob.out" | awk '{print $3}')
bob_holder_key=$(grep 'CT_CHANNEL_HOLDER_KEY=' "$work/bob.out" | cut -d= -f2)
bob_noise_key=$(grep 'CT_CHANNEL_NOISE_KEY=' "$work/bob.out" | cut -d= -f2)

echo "== deriving the real channel id + noise attestations =="
CT_CHANNEL_OPERATOR_PUBKEY="$operator_pub" CT_CHANNEL_BRIDGE_HOLDER="$bob_holder_pub" \
  CT_CHANNEL_HOLDER_KEY="$alice_holder_key" CT_CHANNEL_NOISE_PUBKEY="$alice_noise_pub" \
  "$CT_AGENT_BIN" channel member-material >"$work/alice-material.out"
channel_id=$(grep 'channel_id ' "$work/alice-material.out" | awk '{print $3}')
alice_attest=$(grep 'noise_attestation' "$work/alice-material.out" | awk '{print $3}')

CT_CHANNEL_OPERATOR_PUBKEY="$operator_pub" CT_CHANNEL_BRIDGE_HOLDER="$alice_holder_pub" \
  CT_CHANNEL_HOLDER_KEY="$bob_holder_key" CT_CHANNEL_NOISE_PUBKEY="$bob_noise_pub" \
  "$CT_AGENT_BIN" channel member-material >"$work/bob-material.out"
bob_channel_id=$(grep 'channel_id ' "$work/bob-material.out" | awk '{print $3}')
bob_attest=$(grep 'noise_attestation' "$work/bob-material.out" | awk '{print $3}')
[ "$channel_id" = "$bob_channel_id" ] || { echo "provision.sh: channel id mismatch between alice/bob derivations -- refusing to continue" >&2; exit 1; }
echo "channel_id = $channel_id"

echo "== minting a real OIDC bearer token =="
token=$(curl -sS -X POST "$OIDC_ISSUER/protocol/openid-connect/token" \
  -d "client_id=admin-cli" -d "grant_type=password" \
  -d "username=$OPERATOR_EMAIL" -d "password=$OPERATOR_PASSWORD" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('access_token',''))")
[ -n "$token" ] || { echo "provision.sh: failed to mint an OIDC token -- check A2A_OPERATOR_EMAIL/PASSWORD and A2A_OIDC_ISSUER" >&2; exit 1; }

echo "== registering the channel with the control plane (POST /me/channels) =="
reg_code=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$CP_URL/me/channels" \
  -H "Authorization: Bearer $token" -H 'content-type: application/json' \
  -d "{\"channel\":\"$channel_id\",\"operator_pubkey\":\"$operator_pub\"}")
[ "$reg_code" = "200" ] || { echo "provision.sh: POST /me/channels returned $reg_code (expected 200) -- if this is 404, the control plane's OIDC verifier may be down (see docs.bunsenbrenner.org/reference/api-endpoints/#self-service-channel-registry)" >&2; exit 1; }

echo "== registering agent-alice as a member (POST /me/channels/:channel/members) =="
curl -sS -f -X POST "$CP_URL/me/channels/$channel_id/members" \
  -H "Authorization: Bearer $token" -H 'content-type: application/json' \
  -d "{\"holder\":\"$alice_holder_pub\",\"noise_pubkey\":\"$alice_noise_pub\",\"noise_attestation\":\"$alice_attest\"}" >/dev/null

echo "== registering agent-bob as a member =="
curl -sS -f -X POST "$CP_URL/me/channels/$channel_id/members" \
  -H "Authorization: Bearer $token" -H 'content-type: application/json' \
  -d "{\"holder\":\"$bob_holder_pub\",\"noise_pubkey\":\"$bob_noise_pub\",\"noise_attestation\":\"$bob_attest\"}" >/dev/null

echo "== minting operator-signed grants (1 year; re-run this script to rotate) =="
expires=$(( $(date +%s) + 31536000 ))
alice_grant=$(CT_CHANNEL_OPERATOR_KEY="$operator_key" CT_GRANT_CHANNEL="$channel_id" \
  CT_GRANT_MEMBER_HOLDER="$alice_holder_pub" CT_GRANT_DIRECTION=accept CT_GRANT_EXPIRES="$expires" \
  "$CT_AGENT_BIN" channel grant)
bob_grant=$(CT_CHANNEL_OPERATOR_KEY="$operator_key" CT_GRANT_CHANNEL="$channel_id" \
  CT_GRANT_MEMBER_HOLDER="$bob_holder_pub" CT_GRANT_DIRECTION=initiate CT_GRANT_EXPIRES="$expires" \
  "$CT_AGENT_BIN" channel grant)

echo "== writing $ENV_FILE =="
touch "$ENV_FILE"
for kv in \
  "A2A_CHANNEL_ID=$channel_id" \
  "A2A_ALICE_HOLDER_KEY=$alice_holder_key" \
  "A2A_ALICE_NOISE_KEY=$alice_noise_key" \
  "A2A_ALICE_GRANT=$alice_grant" \
  "A2A_BOB_HOLDER_KEY=$bob_holder_key" \
  "A2A_BOB_NOISE_KEY=$bob_noise_key" \
  "A2A_BOB_GRANT=$bob_grant" \
  "A2A_BOB_HOLDER_PUBKEY=$bob_holder_pub" \
  ; do
  key="${kv%%=*}"
  grep -q "^${key}=" "$ENV_FILE" 2>/dev/null && sed -i "s|^${key}=.*|${kv}|" "$ENV_FILE" || echo "$kv" >>"$ENV_FILE"
done

echo "== done =="
echo "channel_id=$channel_id"
echo "Set A2A_CHANNEL_BROKER/A2A_CHANNEL_RELAY (edge broker/relay host:port) in $ENV_FILE yourself -- this"
echo "script only writes what it actually derived; it never guesses at your plane's edge address."
