#!/usr/bin/env bash
# Provision the REAL Agent-Fabric material for the Nordwind demo, idempotently:
#   * one holder/Noise identity per site (cloud, zentrale, hafenbuero, werkstatt, aussendienst)
#   * one operator key, four channels (see CHANNELS), members registered, grants signed
#   * deploy/generated/<node>.env  (SECRET, mode 600)  +  deploy/generated/nodes.json (public)
# Re-running keeps identities, re-registers (idempotent) and re-signs grants (1 year).
#
#   export NW_CP_URL=https://bunsenbrenner.org
#   export CT_OIDC_TOKEN=...            # or NW_CP_CLIENT_ID/SECRET + NW_OIDC_ISSUER
#   ./nordwind/scripts/nordwind-provision.sh
source "$(dirname "$0")/lib.sh"
need curl; need python3; command -v "$CT_AGENT_BIN" >/dev/null || die "ct-agent not found (set CT_AGENT_BIN)"
umask 077; mkdir -p "$GEN"

NODES="cloud zentrale hafenbuero werkstatt aussendienst"
# name:initiator:acceptor
CHANNELS="cloud-zentrale:cloud:zentrale zentrale-werkstatt:zentrale:werkstatt zentrale-hafenbuero:zentrale:hafenbuero aussendienst-zentrale:aussendienst:zentrale"

for n in $NODES; do
  [ -s "$GEN/identity-$n.env" ] || { "$CT_AGENT_BIN" channel init > "$GEN/identity-$n.env"; echo "identity: $n (new)"; }
done
if [ ! -s "$GEN/operator.env" ]; then "$CT_AGENT_BIN" channel operator-init > "$GEN/operator.env"; echo "operator: new"; fi
OP_KEY="$(envval "$GEN/operator.env" CT_CHANNEL_OPERATOR_KEY)"
OP_PUB="$(grep -o 'operator_pubkey = [0-9a-f]*' "$GEN/operator.env" | awk '{print $3}')"
[ -n "$OP_KEY" ] && [ -n "$OP_PUB" ] || die "operator.env unreadable"
hk() { envval "$GEN/identity-$1.env" CT_CHANNEL_HOLDER_KEY; }
hp() { envval "$GEN/identity-$1.env" CT_CHANNEL_HOLDER_PUBKEY; }
nk() { envval "$GEN/identity-$1.env" CT_CHANNEL_NOISE_KEY; }
np() { envval "$GEN/identity-$1.env" CT_CHANNEL_NOISE_PUBKEY; }
field() { grep -E "^ *$2 " | head -1 | awk '{print $3}'; }

NW_TOKEN="$(nw_token)"
expires=$(( $(date +%s) + 31536000 ))

# node env files start with the identity
for n in $NODES; do
  { echo "NORDWIND_NODE=$n"; echo "NW_HOLDER_KEY=$(hk $n)"; echo "NW_NOISE_KEY=$(nk $n)"; } > "$GEN/$n.env"
done
declare_serve() { :; }
SERVE_zentrale=""; SERVE_hafenbuero=""; SERVE_werkstatt=""
chan_json=""

for spec in $CHANNELS; do
  IFS=: read -r name ini acc <<<"$spec"
  P="NW_CH_$(echo "$name" | tr 'a-z-' 'A-Z_')_"
  mat_i="$(CT_CHANNEL_OPERATOR_PUBKEY=$OP_PUB CT_CHANNEL_BRIDGE_HOLDER=$(hp $acc) CT_CHANNEL_HOLDER_KEY=$(hk $ini) CT_CHANNEL_NOISE_PUBKEY=$(np $ini) "$CT_AGENT_BIN" channel member-material)"
  mat_a="$(CT_CHANNEL_OPERATOR_PUBKEY=$OP_PUB CT_CHANNEL_BRIDGE_HOLDER=$(hp $ini) CT_CHANNEL_HOLDER_KEY=$(hk $acc) CT_CHANNEL_NOISE_PUBKEY=$(np $acc) "$CT_AGENT_BIN" channel member-material)"
  cid="$(echo "$mat_i" | field x channel_id)"; [ "$cid" = "$(echo "$mat_a" | field x channel_id)" ] || die "channel id mismatch for $name"
  att_i="$(echo "$mat_i" | field x noise_attestation)"; att_a="$(echo "$mat_a" | field x noise_attestation)"

  cp_call POST /me/channels "{\"channel\":\"$cid\",\"operator_pubkey\":\"$OP_PUB\"}" >/dev/null || { [ $? = 9 ] || die "register $name failed"; }
  for m in "$ini:$att_i" "$acc:$att_a"; do
    node="${m%%:*}"; att="${m#*:}"
    cp_call POST "/me/channels/$cid/members" "{\"holder\":\"$(hp $node)\",\"noise_pubkey\":\"$(np $node)\",\"noise_attestation\":\"$att\"}" >/dev/null || { [ $? = 9 ] || die "member $node on $name failed"; }
  done
  g_i="$(CT_CHANNEL_OPERATOR_KEY=$OP_KEY CT_GRANT_CHANNEL=$cid CT_GRANT_MEMBER_HOLDER=$(hp $ini) CT_GRANT_DIRECTION=initiate CT_GRANT_EXPIRES=$expires "$CT_AGENT_BIN" channel grant)"
  g_a="$(CT_CHANNEL_OPERATOR_KEY=$OP_KEY CT_GRANT_CHANNEL=$cid CT_GRANT_MEMBER_HOLDER=$(hp $acc) CT_GRANT_DIRECTION=accept CT_GRANT_EXPIRES=$expires "$CT_AGENT_BIN" channel grant)"
  echo "${P}GRANT=$g_i" >> "$GEN/$ini.env"; echo "${P}ID=$cid" >> "$GEN/$ini.env"
  echo "${P}GRANT=$g_a" >> "$GEN/$acc.env"; echo "${P}ID=$cid" >> "$GEN/$acc.env"
  eval "SERVE_$acc=\"\${SERVE_$acc:+\$SERVE_$acc,}$name\""
  chan_json="$chan_json{\"name\":\"$name\",\"a\":\"$ini\",\"b\":\"$acc\",\"id\":\"$cid\"},"
  echo "channel: $name  ${cid:0:12}…  (registered, members + grants ok)"
done
for n in zentrale hafenbuero werkstatt; do eval "echo NW_SERVE=\$SERVE_$n" >> "$GEN/$n.env"; done

python3 - "$GEN" "${chan_json%,}" <<'PY'
import json, sys, re, os
gen, chans = sys.argv[1], json.loads("[" + sys.argv[2] + "]")
def pub(n):
    t = open(f"{gen}/identity-{n}.env").read()
    return re.search(r"CT_CHANNEL_HOLDER_PUBKEY=([0-9a-f]{64})", t).group(1)
defs = [
 ("cloud",        "Cloud-Kern",              "Cloud core",       "Linux",   "cloud",  False),
 ("zentrale",     "Zentrale Hamburg",        "HQ Hamburg",       "macOS",   "hub",    False),
 ("hafenbuero",   "Hafenbüro Bremen",        "Harbour office Bremen", "Linux", "site", False),
 ("werkstatt",    "Werkstatt (GPU)",         "Workshop (GPU)",   "Linux",   "gpu",    False),
]
# The Außendienst node is described as what it really is. Default: a Linux field node (container,
# outbound-only like a phone). Set NW_AUSSENDIENST_KIND=android only for a real Android/Termux device.
if os.environ.get("NW_AUSSENDIENST_KIND", "linux") == "android":
    defs.append(("aussendienst", "Außendienst (Android)", "Field service (Android)", "Android", "mobile", True))
else:
    defs.append(("aussendienst", "Außendienst (Feldknoten)", "Field service (field node)", "Linux", "site", False))
nodes = [dict(id=i, label_de=d, label_en=e, holder_pubkey=pub(i), os_hint=o, role=r, planned=p) for i, d, e, o, r, p in defs]
json.dump(dict(nodes=nodes, channels=chans), open(f"{gen}/nodes.json", "w"), indent=2, ensure_ascii=False)
PY
chmod 600 "$GEN"/*.env
echo; echo "done. Secrets: $GEN/*.env (mode 600, git-ignored). Public manifest: $GEN/nodes.json"
echo "next: ./nordwind/scripts/nordwind-seed-topology.sh"
