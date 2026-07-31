# CADS-a2a-demo — a real Agent-Fabric channel call, live in the browser

`https://a2a-demo.bunsenbrenner.org` (once deployed) watches a genuine direct-address
Agent-Fabric channel call happen between two independent `ct-agent channel` OS
processes — the exact mechanism
[docs.bunsenbrenner.org's "Your first Agent-Fabric channel"](https://docs.bunsenbrenner.org/tutorials/first-channel/)
tutorial walks through by hand. This bridge is just what clicks the buttons a human
would: it spawns the responder (`agent-alice`), parses the real cert it prints, spawns
the initiator (`agent-bob`) with **your own typed message** on stdin, and streams
every real step to the dashboard over Server-Sent Events.

## What's real, what's simulated

- **Real**: two genuinely separate OS processes each round (direct-address is
  single-shot by design, confirmed in the docs — so a fresh responder is spawned every
  time), a real Noise_IK handshake, your exact message piped over the real channel, and
  the handler's real reply (its own PID and timestamp) streamed back.
- **Simulated**: nothing about the call itself. The only "demo" concession is both
  identities living in one container for convenience — a real deployment runs
  agent-alice and agent-bob on two different machines that never share a filesystem or
  process tree.

## Running it locally (no tunnel needed)

`docker compose up <one-service>` does **not** work against this compose file
(Compose interpolates every service's required env vars up front — same real gotcha
documented in CADS-auction-demo's README). Build and run the bridge directly instead:

```
docker build -t a2a-demo-bridge .
docker run --rm -p 8790:8790 a2a-demo-bridge
```
then open `http://127.0.0.1:8790/`, type a message, and click **Send over the channel**.

## Verifying the wiring without a browser

```
curl -N http://127.0.0.1:8790/events &
curl -X POST http://127.0.0.1:8790/run -H 'content-type: application/json' \
  -d '{"message":"what is the airspeed velocity of an unladen swallow?"}'
```

## Hermetic build/test

```
docker run --rm -v "$PWD":/work -w /work \
  -v cads-a2a-demo-cargo-registry:/usr/local/cargo/registry \
  -e RUSTFLAGS='-D warnings' rust:1-slim-bookworm bash -c 'cargo test'
```

## Two real bugs found and fixed while verifying this end to end

- `ct-agent channel`'s own `ct-agent channel: ...` status lines (including the one
  naming the responder's cert) print to **stderr**, not stdout — the bridge originally
  read only stdout and hung forever waiting for a line that would never arrive there.
  Confirmed by redirecting each stream to a separate file and inspecting both directly.
- `handler.sh` originally used `set -eu`. `ct-agent` feeds the caller's request to the
  handler's stdin without a guaranteed trailing newline, so POSIX `read -r` hitting EOF
  right after the last byte returns non-zero even though it captured the value
  correctly — under `set -e` that aborted the script before it printed anything
  (`ct-agent channel` logged `service handler exited exit status: 1`). Removed
  `set -e`, matching the docs' own verified handler script exactly.

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
ACME client or holds a DNS credential.

## Layout

- `src/main.rs` — the bridge: spawns real `ct-agent channel` processes via
  `tokio::process`, streams every real lifecycle event over SSE, serves the dashboard.
  No `ct-common`/`ct-agent` crate dependency at all — it only shells out to the actual
  `ct-agent` binary, exactly as a human operator would from a terminal.
- `handler.sh` — the responder's real answer script (word-reverse + own PID/timestamp,
  so a viewer can tell the reply came from a genuinely distinct process).
- `Dockerfile` — multi-stage: builds the real `ct-agent` (from its own repo) **and**
  this repo's bridge into one image, since the bridge needs `ct-agent` on `PATH` to
  spawn it internally.
- `Agent.Dockerfile` / `Caddy.Dockerfile` / `Caddyfile` / `compose.a2a-demo.yml` /
  `run-demo.sh` — the standalone Browser-Plane publishing pattern (a **second**,
  unrelated use of `ct-agent`: the one that tunnels the dashboard itself to a public
  subdomain), copied from CADS-Tunnel's `examples/help-site/` / CADS-auction-demo.
