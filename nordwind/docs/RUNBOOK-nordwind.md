# Runbook: Nordwind Firmennetz (a2a-demo.bunsenbrenner.org)

Deployment is a lead/operator task. Nothing here is executed by the PR. Steps marked **[OP]** need an
operator credential that the repo never holds.

## 0. What runs where

| Node | Host | Runs | Inbound ports |
|---|---|---|---|
| Cloud-Kern (hub + dashboard) | ssh-united.becke.biz | `compose.hub.yml`: nordwind-hub, Caddy origin (login gate), tunnel agent | none (tunnel) |
| Hafenbüro | ssh-united.becke.biz | `compose.nordwind.yml --profile hafenbuero` | none |
| Werkstatt (GPU, Ollama) | ssh.labor-setup.com | `compose.nordwind.yml --profile werkstatt` + Ollama on the host | none |
| Zentrale (macOS) | ssh.cs4f.org | native `node-run.sh` (section 4) | none |
| Außendienst (Android) | phone | planned; identity + channel are already provisioned | none |

Every node dials out to `bunsenbrenner.org` (broker 4435/udp, relay 4436/udp, front door 443/tcp).

## 1. Prerequisites [OP]

1. A Keycloak **service account** for the account that will own the topology and the channels
   (portal: *Service accounts*, or `POST /me/service-accounts {"name":"nordwind-demo"}` with a
   human token; the secret is shown once). Reason: the hub must read `/me/topologies` unattended and
   the control plane identifies the owner by token subject. Alternative for a one-off seeding: a
   human `CT_OIDC_TOKEN` (`ct-agent login`), but then the hub cannot read the result unattended.
2. `ct-agent` on the machine that runs the provisioning (`cargo install --git https://github.com/scimbe/ct-agent --tag v0.7.24 ct-agent`
   or any release binary).
3. A single-use tunnel **join token** for hostname `a2a-demo.bunsenbrenner.org`: only if the existing
   tunnel agent volume is replaced. Re-using the running `cads-a2a-demo-a2a-demo-agent-1` state
   volume needs no new token.
4. The login gate for the hostname: portal -> tunnel `a2a-demo` -> *require login* **on**, add the
   allowed e-mails (or "any login"). Without this the hub answers 403 to everyone (fail closed).

## 2. Provision channels and topology (once, from any trusted machine)

```bash
cd CADS-a2a-demo
export NW_CP_URL=https://bunsenbrenner.org
export NW_OIDC_ISSUER=https://auth.bunsenbrenner.org/realms/ct-demo
export NW_CP_CLIENT_ID=sa-...      NW_CP_CLIENT_SECRET=...      # [OP] the service account
export CT_AGENT_BIN=$(command -v ct-agent)
./nordwind/scripts/nordwind-provision.sh          # identities, 4 channels, members, grants -> nordwind/deploy/generated/
NW_SHARE_EMAILS=you@example.org ./nordwind/scripts/nordwind-seed-topology.sh   # topology in that account, shared by e-mail
```

Both scripts are idempotent. `deploy/generated/` is git-ignored: `*.env` hold private keys (mode 600),
`nodes.json` is public. Copy **only** the matching file to each host:

```bash
scp nordwind/deploy/generated/hafenbuero.env becke@ssh-united.becke.biz:CADS-a2a-demo/nordwind/deploy/generated/
scp nordwind/deploy/generated/werkstatt.env  <user>@ssh.labor-setup.com:CADS-a2a-demo/nordwind/deploy/generated/
scp nordwind/deploy/generated/zentrale.env   <user>@ssh.cs4f.org:CADS-a2a-demo/nordwind/deploy/generated/
scp nordwind/deploy/generated/{cloud.env,nodes.json} becke@ssh-united.becke.biz:CADS-a2a-demo/nordwind/deploy/generated/
```

## 3. Linux nodes (Hafenbüro, Werkstatt)

```bash
git clone https://github.com/scimbe/CADS-a2a-demo && cd CADS-a2a-demo && git checkout <merged ref>
# Hafenbüro (ssh-united):
docker compose -f nordwind/deploy/compose.nordwind.yml --profile hafenbuero up -d --build
# Werkstatt (ssh.labor-setup.com): Ollama must run on the host and have the model:
ollama pull llama3.2          # or export NORDWIND_OLLAMA_MODEL=<installed model>
export NORDWIND_GPU_LABEL="<GPU name>"       # optional, shown with the result
docker compose -f nordwind/deploy/compose.nordwind.yml --profile werkstatt up -d --build
docker compose -f nordwind/deploy/compose.nordwind.yml logs -f      # expect: "serving zentrale-werkstatt"
```

Ollama must be reachable from the container at `NORDWIND_OLLAMA_URL` (default
`http://host.docker.internal:11434`; Ollama listening on 127.0.0.1 only is not reachable from a
container: set `OLLAMA_HOST=0.0.0.0:11434` for the host service **and** firewall it, or run the
Werkstatt natively like the Zentrale in section 4).

## 4. Zentrale (macOS, native)

```bash
# once: ct-agent v0.7.24+ and python3 (Xcode CLT) on the Mac
cargo install --git https://github.com/scimbe/ct-agent --tag v0.7.24 --locked ct-agent
cd CADS-a2a-demo
NORDWIND_ENV_FILE=$PWD/nordwind/deploy/generated/zentrale.env ./nordwind/node/node-run.sh
```

Keep it running with `launchctl`/`tmux`. `node-run.sh` supervises one acceptor per served channel
(`cloud-zentrale`, `aussendienst-zentrale`). The dashboard shows the OS from the node's own
`ping` reply, so a Docker-on-macOS Zentrale would honestly appear as Linux.

## 5. Hub + dashboard (ssh-united)

```bash
cp nordwind/deploy/hub.env.example nordwind/deploy/hub.env   # fill: NW_CP_CLIENT_ID/SECRET, NW_OIDC_ISSUER, A2A_AGENT_EDGE, NW_JOIN_TOKEN [OP]
docker compose -f nordwind/deploy/compose.hub.yml --env-file nordwind/deploy/hub.env up -d --build
```

Switching from the old demo: the new stack uses the same hostname, so stop the old
`cads-a2a-demo-*` stack **after** the new agent has the tunnel (`docker compose -f compose.a2a-demo.yml down`).
Then change the gallery card from "bald verfügbar" to live.

## 6. Verify

```bash
curl -si https://a2a-demo.bunsenbrenner.org/api/state | head -1     # expect 302/403 without login
# in a browser: log in via bunsenbrenner.org, open the page; nodes turn green within ~30 s; press the button
docker compose -f nordwind/deploy/compose.hub.yml logs nordwind-hub | tail
```

## 7. Local proof without any account

```bash
cargo build --bin nordwind-hub
CT_AGENT_BIN=$(command -v ct-agent) python3 nordwind/scripts/nordwind-local-lab.py   # http://127.0.0.1:8791/
CT_AGENT_BIN=$(command -v ct-agent) cargo test                                       # incl. real-process integration test
(cd nordwind/node && python3 -m unittest -q test_nordwind_node)
```

## 8. Operational notes

- If nodes pair badly (mixed QUIC/443 pairers, see ct-agent `docs/channel.md`, "Pairing reality"),
  set `NW_FRONT_DOOR_ONLY=1` on **all** nodes and the hub.
- Rotating a grant: re-run `nordwind-provision.sh`, redistribute the env files, restart the services.
- The hub never receives node secrets; the nodes never receive control-plane credentials.
