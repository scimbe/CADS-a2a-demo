# Nordwind Firmennetz: verteilte Agenten, live in der Topologie des Kontos

`nordwind-hub` (dieses Repo, `src/bin/nordwind_hub.rs`, `web/`) zeigt die Topologie **aus dem Konto**
(`/me/topologies` der Control Plane) und fuehrt das Szenario "Toern-Briefing Samstag Bremen" mit echten
Channel-Calls aus: Cloud-Kern -> Zentrale -> Werkstatt (lokales LLM) -> Hafenbuero (Route/Wetter) -> Ergebnis.
Jeder Standort ist ein eigener `ct-agent` mit eigenem Holder-Key, ohne offene Ports; der Edge sieht nur Chiffretext.

Betrieb und Befehle: [`nordwind/docs/RUNBOOK-nordwind.md`](nordwind/docs/RUNBOOK-nordwind.md).
Lokal ohne Konto (echte Prozesse, direkte Adressen): `python3 nordwind/scripts/nordwind-local-lab.py`.

**Login.** Zugriff nur mit bunsenbrenner-Login: Caddy `forward_auth` auf `https://bunsenbrenner.org/gate/check`
(Keycloak-OIDC, pro Hostname gepflegte E-Mail-Allowlist), Header `X-Gate-Email`. Gewaehlt, weil bereits produktiv
(marketplace-admin, webconference), keine neue Keycloak-Client-/Redirect-Konfiguration noetig und keine eigenen
Sessions im Hub. Der Hub ist fail-closed: ohne nicht-leeres `X-Gate-Email` antwortet er 403 (das Gate liefert 200 mit
leerem Header, wenn "login required" fuer den Host aus ist). Einzige offene Routen: `/healthz` und `/node/hop/<run>`
(Capability-URL, Zufallstoken pro Lauf, nur waehrend des Laufs gueltig, hart validiert).

**Topologie-Quelle.** Das Gate liefert nur die E-Mail, kein Bearer-Token; die Control Plane liefert geteilte Topologien
nur an eine Portal-Session aus. Der Hub liest deshalb die Topologie des Demo-Kontos (Service Account, Client-Credentials)
und zeigt sie jedem angemeldeten Betrachter. `nordwind-seed-topology.sh` teilt sie per E-Mail, damit sie auch im
Portal-Editor der Beteiligten erscheint. Faellt die Control Plane aus, zeigt die Seite die lokale Vorlage und sagt das.

**Echt vs. Anzeige.** Online-Zustand, Latenz, Betriebssystem und Bytes stammen ausschliesslich aus echten Calls
(`mesh_status`: Zentrale pingt Werkstatt/Hafenbuero; Hop-Bytes = vom Agenten unterhalb von Noise gezaehlte Chiffretext-Bytes).
Live-Hop-Meldungen von der Zentrale sind ein Seitenkanal fuer die Anzeige; die verbindliche Spur steckt in der
verschluesselten Antwort (Tag `trace` statt `live`). Faellt LLM oder Wetterquelle aus, scheitert der Hop sichtbar.

**Ereignisse** (`GET /api/events`, SSE, JSON je Zeile; Snapshot `GET /api/state`, Graph `GET /api/topology`):
`run_start{run,topic}` · `hop{run,seq,from,to,channel,service,state:start|ok|error,dur_ms,sent_bytes,recv_bytes,plain_out,plain_in,transport,source:hub|node-report|trace,error?}` ·
`run_done{run,total_ms,briefing}` · `run_error{run,error}` · `graph{}` (neu laden). Knoten-Keys: `cloud, zentrale, hafenbuero, werkstatt, aussendienst`.
Start: `POST /api/scenario`. Tests: `cargo test` (inkl. Integrationstest mit echten ct-agent-Prozessen, `CT_AGENT_BIN` setzen),
`(cd nordwind/node && python3 -m unittest -q test_nordwind_node)`, `python3 nordwind/scripts/test_seed_topology.py`.

---

# CADS-a2a-demo — a real Agent-Fabric channel call, live in the browser

`https://a2a-demo.bunsenbrenner.org` (once deployed) watches a genuine **broker-mediated**
Agent-Fabric channel call happen between two genuinely separate containers —
`agent-alice` (`a2a-demo-alice`, a persistent, independently-deployed process) and
`agent-bob` (spawned by the bridge, per visitor request, as its own OS process) — the
exact mechanism [docs.bunsenbrenner.org's "Set up an Agent-Fabric
channel"](https://docs.bunsenbrenner.org/how-to/join-a-channel/) and ["Serve a callable
service over a channel"](https://docs.bunsenbrenner.org/how-to/serve-a-channel-service/)
describe, click-tested live against the real production edge while building this
rework (see below). Neither container ever has the other's address, container name, or
filesystem — both reach each other only through the shared plane's real edge broker/relay
(`CT_CHANNEL_BROKER`/`CT_CHANNEL_RELAY`), the same infrastructure any two independently
operated agents would use. This bridge is just what clicks the buttons a human would: it
dials agent-bob's own process with **your own typed message** on stdin and streams every
real step to the dashboard over Server-Sent Events.

## What's real, what's simulated

- **Real**: two genuinely separate containers, connected only through the real
  production edge's broker/relay — no shared filesystem, no shared process tree, no
  local address either one dials directly. A real channel, registered once with the
  control plane (`POST /me/channels`, `POST /me/channels/:channel/members`) via a real
  OIDC-authenticated account, with real operator-signed grants. A real Noise handshake,
  your exact message piped over the real channel, and agent-alice's real reply (its own
  PID and timestamp, from inside its own container) streamed back. `agent-alice` is
  persistent (broker-mediated `CT_CHANNEL_SERVE=1`, admits a fresh peer indefinitely,
  #200) — it is not respawned per round.
- **Simulated**: nothing about the call, the containers, or the channel. The only thing
  generated per-round is the task text itself (typed by the visitor) — everything the
  mechanism does with it is real.

## Provisioning the real channel (once, ahead of time)

`provision.sh` mints agent-alice's and agent-bob's real Agent-Fabric identities, derives
their real channel id, registers the channel + both members with the control plane over
a real OIDC bearer token, mints two real operator-signed grants, and writes everything
`compose.a2a-demo.yml` needs into `.env`. This is a one-time (or rotate-on-demand) step —
a normal demo round never calls it, `src/main.rs` only ever dials with the identity this
script already wrote:

```
A2A_CP_URL=https://<your plane> \
A2A_OIDC_ISSUER=https://<auth host>/realms/<realm> \
A2A_OPERATOR_EMAIL=<an account on that plane> A2A_OPERATOR_PASSWORD=<its password> \
  ./provision.sh
```

Then set `A2A_CHANNEL_BROKER`/`A2A_CHANNEL_RELAY` in `.env` yourself (the plane's edge
broker/relay host:port, e.g. `edge:4435`/`edge:4436` on the shared compose network) —
`provision.sh` derives everything it can prove, but never guesses at your plane's edge
address.

This is the demo-operator path — one account provisions both sides. If you're one of the real
participants (Alice, bob-1, bob-2) and want to set up your own channel with a peer directly,
with no operator/core involvement at all — nothing exchanged beyond an email address and a
public key — see [SELF-ORGANIZE.md](SELF-ORGANIZE.md), live-verified end to end.

## Running it

```
docker compose -f compose.a2a-demo.yml --env-file .env up --build -d a2a-demo-alice a2a-demo-bridge
```

then open `http://127.0.0.1:8790/` (or wherever you've published it), type a message,
and click **Send over the channel**. `docker compose up <one-service>` does **not**
work against this compose file (Compose interpolates every service's required env vars
up front — same real gotcha documented in CADS-auction-demo's README) — bring up both
`a2a-demo-alice` and `a2a-demo-bridge` together, or all four services for the full
public path.

## Verifying the wiring without a browser

```
curl -N http://127.0.0.1:8790/events &
curl -X POST http://127.0.0.1:8790/run -H 'content-type: application/json' \
  -d '{"message":"what is the airspeed velocity of an unladen swallow?"}'
```

Real output from the actual rework verification pass (against the real production
edge, agent-alice in her own container, pid genuinely her own):

```
data: {"type":"round_start","round":1,"message":"..."}
data: {"type":"initiator_dialing","broker":"edge:4435"}
data: {"type":"channel_connected","line":"ct-agent channel: plane-brokered Initiate (relay 172.18.0.6:4436)"}
data: {"type":"initiator_log","line":"ct-agent channel: --call-service text_generation (one service call over the channel, then exit)"}
data: {"type":"channel_connected","line":"ct-agent channel: peer is relay-only (no dialable address) — using the edge relay (#121)"}
data: {"type":"reply_received","reply":"agent-alice (pid 14, 08:51:53): you said \"...\" -- reversed: \"...\""}
data: {"type":"round_done","round":1}
```

## Hermetic build/test

```
docker run --rm -v "$PWD":/work -w /work \
  -v cads-a2a-demo-cargo-registry:/usr/local/cargo/registry \
  -e RUSTFLAGS='-D warnings' rust:1-slim-bookworm bash -c 'cargo test'
```

## Real bugs found and fixed while building this

- `ct-agent channel`'s own `ct-agent channel: ...` status lines print to **stderr**,
  not stdout — an earlier version of the bridge read only stdout and hung forever
  waiting for a line that would never arrive there. Confirmed by redirecting each
  stream to a separate file and inspecting both directly.
- The handler script must not use `set -eu`. `ct-agent` feeds the caller's request to
  the handler's stdin without a guaranteed trailing newline, so POSIX `read -r` hitting
  EOF right after the last byte returns non-zero even though it captured the value
  correctly (`ct-agent channel` logged `service handler exited exit status: 1`).
- **Production bug, found while provisioning this demo's real channel**: `POST
  /me/channels` returned `404` against the live plane even with a freshly-minted, valid
  OIDC token. Root cause: the running `control-plane` container had
  `CT_OIDC_ISSUER=https://keycloak.example/realms/CADS-Tunnel` — a leftover
  `.env.example` placeholder, not the real issuer — because an earlier redeploy had
  dropped the `compose.sso.yml` overlay for that service. Fixed by redeploying
  `control-plane` with the full `-f compose.selfhost.yml -f compose.frontdoor.yml -f
  compose.sso.yml` overlay; this was a platform-wide `/me/*` outage, not specific to
  this demo (see docs.bunsenbrenner.org's own documented `/me/*` outage caveat, which
  is what pointed at the cause).
- The first attempt at a real broker-mediated round-trip, dialed against the plane's
  public hostname from the plane's own host, stalled with `channel join admission
  exchange stalled (#140)` on both sides. Root cause: connecting to your own public IP
  from the same host (hairpin NAT) — not a bug in the channel mechanism. Confirmed by
  retrying the identical exchange over `127.0.0.1` (still real: same broker/relay
  process, same registered channel, same grants), which worked immediately. Inside the
  actual deployed topology this never applies — both containers reach `edge` by its
  compose network name, not the public hostname.

## Publishing it live (Browser Plane)

```
A2A_CERT_DIR=<dir with fullchain.pem+privkey.pem, issued CORE-side> \
CP_URL=<control-plane URL> EDGE=<edge host:port> \
  ./run-demo.sh up
./run-demo.sh status
./run-demo.sh down
```

Same pattern as `CADS-auction-demo/run-demo.sh` — mints a single-use join token, brings
up the bridge + Caddy origin + a Browser-Plane `ct-agent`, polls until 200. The TLS
certificate is issued CORE-side (deSEC DNS-01) and relayed in; this repo never runs an
ACME client or holds a DNS credential. This is a **second, unrelated** use of
`ct-agent` — the one that tunnels the *dashboard itself* to a public subdomain, not the
Agent-Fabric channel the dashboard visualizes.

## Self-hosting: running your own instance (not on the operator's host)

Sections above this one need zero CADS-Tunnel plane (local Docker Compose only,
against the two direct-address `ct-agent channel` processes this repo already
spawns). Only "Publishing it live" needs a plane, and it doesn't have to be the
operator's:

1. **Your own CADS-Tunnel plane** — `./scripts/deploy-selfhost.sh --frontdoor`
   in a `CADS-Tunnel` checkout (see its
   [`docs/ops/runbook.md`](https://github.com/scimbe/CADS-Tunnel/blob/main/docs/ops/runbook.md)).
   Generic, not tied to the operator's domain/account: set `DESEC_TOKEN` and
   `PORTAL_PUBLIC_HOST` to **your own** domain (deSEC is free and works with any
   domain you own, or even a free `yourname.dedyn.io` name — see
   [`docs/dns01-desec.md`](https://github.com/scimbe/CADS-Tunnel/blob/main/docs/dns01-desec.md)).
2. **A cert for your own subdomain** (e.g. `a2a-demo.yourdomain.tld`) — same
   deSEC DNS-01 mechanism your plane's front door already uses, or any ACME
   method you prefer — into a local `fullchain.pem`/`privkey.pem` dir.
3. **Run it against your plane, not the operator's:**
   ```
   A2A_CERT_DIR=/path/to/your/cert-dir \
   HOSTNAME_FQDN=a2a-demo.yourdomain.tld \
   CP_URL=http://<your-plane-host>:8090 EDGE=<your-plane-host>:4433 \
     ./run-demo.sh up
   ```
   (`run-demo.sh`'s own header comment documents every override var.)
4. Point DNS for `a2a-demo.yourdomain.tld` at your plane's host, then verify
   with the same checks in "Verifying the wiring" above, against your own URL.

Once this runs stably end to end on your own infrastructure, the operator's copy
can be taken down.

## Layout

- `src/main.rs` — the bridge: on each visitor request, spawns agent-bob's own real
  `ct-agent channel` process via `tokio::process` (broker-mediated, real fixed
  identity/grant from `.env`), streams every real lifecycle event over SSE, serves the
  dashboard. No `ct-common`/`ct-agent` crate dependency — it only shells out to the
  actual `ct-agent` binary, exactly as a human operator would from a terminal.
- `provision.sh` — the one-time real channel/identity/grant provisioning step (see
  above).
- `handler-alice.sh` — agent-alice's real answer script (word-reverse + her own PID/
  timestamp, so a viewer can tell the reply came from her genuinely distinct
  container), baked into `Alice.Dockerfile`'s own image, never touched by the bridge.
- `Dockerfile` — builds the real `ct-agent` (from its own repo) **and** this repo's
  bridge into one image, since the bridge needs `ct-agent` on `PATH` to spawn
  agent-bob's process. Does **not** include agent-alice's handler or identity.
- `Alice.Dockerfile` — agent-alice's own, completely separate image: the real
  `ct-agent` binary plus `handler-alice.sh`, run as `ct-agent channel` directly
  (broker-mediated, persistent serve mode).
- `Agent.Dockerfile` / `Caddy.Dockerfile` / `Caddyfile` / `compose.a2a-demo.yml` /
  `run-demo.sh` — the standalone Browser-Plane publishing pattern (a **third**,
  unrelated use of `ct-agent`: the one that tunnels the dashboard itself to a public
  subdomain), copied from CADS-Tunnel's `examples/help-site/` / CADS-auction-demo.
