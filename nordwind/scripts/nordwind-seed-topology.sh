#!/usr/bin/env bash
# Create (or complete) the Nordwind topology in the account behind the token. Idempotent:
# an existing topology that already contains one of the Nordwind agents is reused, missing
# agents/labels/edges/channel links are added, nothing is duplicated or removed.
#
# Needs deploy/generated/nodes.json from nordwind-provision.sh. Creates NO account.
#
#   export NW_CP_URL=https://bunsenbrenner.org
#   export CT_OIDC_TOKEN=...      # or NW_CP_CLIENT_ID/SECRET + NW_OIDC_ISSUER (the demo service account)
#   export NW_SHARE_EMAILS=alice@example.org,bob@example.org   # optional: visible in their portal editor
#   ./nordwind/scripts/nordwind-seed-topology.sh
source "$(dirname "$0")/lib.sh"
need curl; need python3
[ -s "$GEN/nodes.json" ] || die "$GEN/nodes.json missing - run nordwind-provision.sh first"
NW_TOKEN="$(nw_token)" NW_CP_URL="$NW_CP_URL" NW_MANIFEST="$GEN/nodes.json" NW_SHARE_EMAILS="${NW_SHARE_EMAILS:-}" \
NW_TOPOLOGY_STATE="$GEN/topology.json" python3 - <<'PY'
import json, os, sys, urllib.request, urllib.error
base, tok = os.environ["NW_CP_URL"].rstrip("/"), os.environ["NW_TOKEN"]
man = json.load(open(os.environ["NW_MANIFEST"]))

def call(method, path, body=None, ok=(200,), tolerate=()):
    req = urllib.request.Request(base + path, method=method, headers={"Authorization": "Bearer " + tok, "content-type": "application/json"},
                                 data=None if body is None else json.dumps(body).encode())
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw.strip().startswith(("{", "[")) else raw)
    except urllib.error.HTTPError as e:
        if e.code in tolerate:
            return e.code, e.read().decode()
        sys.exit(f"HTTP {e.code} {method} {path}: {e.read().decode()[:200]}")

holders = {n["holder_pubkey"]: n for n in man["nodes"]}
tops = call("GET", "/me/topologies")[1]
views = [call("GET", f"/me/topologies/{t['id']}")[1] for t in tops]
mine = next((v for v in views if any(a["id"] in holders for a in v["agents"])), None)
if mine is None:
    tid = call("POST", "/me/topologies")[1]["id"]
    mine = call("GET", f"/me/topologies/{tid}")[1]
    print("topology: created", tid)
else:
    print("topology: reusing", mine["id"])
tid = mine["id"]
have = {a["id"]: a for a in mine["agents"]}
for n in man["nodes"]:
    kind = "super-peer" if n["role"] == "hub" else "peer"
    label = n["label_de"] + (" (geplant)" if n.get("planned") else "")
    if n["holder_pubkey"] not in have:
        code, res = call("POST", f"/me/topologies/{tid}/agents", {"agent": n["holder_pubkey"], "kind": kind, "label": label}, tolerate=(409, 403))
        if code in (409, 403):
            sys.exit(f"agent {n['id']} cannot be assigned ({code}): it belongs to another topology/owner. {res}")
        print("agent: added", n["id"])
    elif have[n["holder_pubkey"]].get("label") != label:
        call("PUT", f"/me/topologies/{tid}/agents/{n['holder_pubkey']}/label", {"label": label})
        print("agent: relabelled", n["id"])
edges = {frozenset((e["a"], e["b"])): e for e in mine["edges"]}
pub = {n["id"]: n["holder_pubkey"] for n in man["nodes"]}
for c in man["channels"]:
    a, b = pub[c["a"]], pub[c["b"]]
    if frozenset((a, b)) not in edges:
        call("POST", f"/me/topologies/{tid}/edges", {"a": a, "b": b}, tolerate=(409,))
        print("edge: added", c["name"])
    if (edges.get(frozenset((a, b))) or {}).get("channel") != c["id"]:
        call("PUT", f"/me/topologies/{tid}/edges/channel", {"a": a, "b": b, "channel": c["id"]}, tolerate=(403, 404))
for em in [e.strip() for e in os.environ["NW_SHARE_EMAILS"].split(",") if e.strip()]:
    call("POST", f"/me/topologies/{tid}/share", {"email": em})
    print("shared with", em)
json.dump({"topology_id": tid}, open(os.environ["NW_TOPOLOGY_STATE"], "w"))
final = call("GET", f"/me/topologies/{tid}")[1]
print(f"ok: topology {tid}: {len(final['agents'])} agents, {len(final['edges'])} edges")
PY
