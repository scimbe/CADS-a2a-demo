# How Alice, bob-1, and bob-2 can set up their own channels — no operator, no core

`provision.sh` (see the README) is the demo-operator path: one account mints identities for
*both* sides and wires up `compose.a2a-demo.yml`. This page is the other path — for the three
real participants who want to connect to each other directly, on their own, without asking
whoever runs this demo's core infrastructure to do it for them.

**The whole idea in one sentence:** the only things two people ever need to send each other are
their **email address** and their **holder public key** (their agent-fabric "network identity" —
not a secret, safe to paste anywhere). Nothing else crosses between them. No operator private
key, no channel id has to be handed over, no admin token, no core mediation.

This isn't a proposal — it was run for real against production on 2026-08-01, with two
throwaway accounts standing in for two independent participants, proven end to end including a
real message crossing a real edge relay in both directions. The steps below are exactly what was
run, generalized to the three named roles this demo actually uses.

## Why this works: what's actually needed to connect two people

A channel id isn't assigned by a server — it's *derived*, deterministically, from three public
values: the channel's operator public key, and both members' holder public keys
(`channel_id_for_link`, order-independent). Anyone who knows those three values computes the
same channel id independently. That's the whole trick: once two people have exchanged their
public identities, they don't need a third party to tell them what channel they're on.

What a channel *membership* needs beyond that — a member's `noise_pubkey` plus a
self-signed attestation binding it to their holder key — is also something each person computes
**locally**, from their own private key, and submits themselves via the portal's self-service
claim. Nobody else ever touches it.

The one piece that has to come from *someone* specific is the operator-signed grant
(`CT_CHANNEL_GRANT`) — cryptographic proof "this holder may join this channel." That's not
bureaucracy, it's the actual authorization. But "the operator" doesn't have to mean the demo's
core maintainer — it just means *whoever registered this particular channel*, which can be any
one of the three of you, using nothing but your own portal account.

One real constraint found while proving this out, worth being upfront about: there is currently
no API to *list* a channel's members, so the operator can't look up a self-claimed member's
holder_pubkey afterward to mint their grant — they have to already have it. That's why the
exchange below is **two-way**: each side sends the other their email *and* their holder_pubkey,
not just one or the other.

## Setup, per pair

CADS-a2a-demo needs three separate channels — alice↔bob-1, alice↔bob-2, bob-1↔bob-2 — because a
channel is always between exactly two holders, never a group. Alice is naturally the operator for
the two pairs she's in (she's already doing this via `provision.sh`, just using her own account
instead of the demo's). Nothing stops bob-1 and bob-2 from setting up their own pair the same
way, entirely without Alice or core.

Pick **one** side per pair to be that pair's operator — doesn't matter which, the steps are
symmetric except for who runs the "operator" half.

### 1. Each side generates their own identity, locally

```bash
ct-agent channel init
```

```
# Agent-Fabric channel member identity — generated locally, keep the key secret.
export CT_CHANNEL_HOLDER_KEY=<64 hex — SECRET, never send this anywhere>
export CT_CHANNEL_NOISE_KEY=<64 hex — SECRET, never send this anywhere>
#   holder_pubkey = <64 hex — safe to share>
#   noise_pubkey  = <64 hex — safe to share>
```

Save the whole block somewhere durable (a password manager, an env file you control) — the two
`CT_CHANNEL_*_KEY` lines are the only genuinely secret material in this entire process, and they
never leave the machine that generated them.

### 2. Exchange email + holder_pubkey — both directions

Whatever channel you already have (chat, email, this GitHub thread): each side sends the other
their **email address** and their **holder_pubkey** from step 1. That's it — two short hex/text
values each way, nothing secret.

### 3. The operator side: register the channel

```bash
ct-agent channel operator-init
```

```
#   operator_pubkey = <64 hex — safe to share>
export CT_CHANNEL_OPERATOR_KEY=<64 hex — SECRET, keep this one durably>
```

Derive the channel id (order-independent — either side could run this, the operator runs it
here since they need it to register):

```bash
CT_CHANNEL_OPERATOR_PUBKEY=<your new operator_pubkey> \
CT_CHANNEL_BRIDGE_HOLDER=<the OTHER side's holder_pubkey, from step 2> \
CT_CHANNEL_HOLDER_KEY=<your own private holder key, from step 1> \
CT_CHANNEL_NOISE_PUBKEY=<your own noise_pubkey, from step 1> \
ct-agent channel member-material
```

This prints `channel_id`, plus your own `noise_attestation`. Register the channel and yourself
as a member, then allow-list the other side's email so they can self-claim:

```bash
TOKEN=<your own OIDC bearer token — log into the portal, or mint one against your account>

curl -X POST https://bunsenbrenner.org/me/channels \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"channel":"<channel_id>","operator_pubkey":"<your operator_pubkey>"}'

curl -X POST https://bunsenbrenner.org/me/channels/<channel_id>/members \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"holder":"<your holder_pubkey>","noise_pubkey":"<your noise_pubkey>","noise_attestation":"<from member-material above>"}'

curl -X POST https://bunsenbrenner.org/me/channels/<channel_id>/allowlist \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"email":"<the other side'\''s email>"}'
```

Send the other side your **operator_pubkey** and your **holder_pubkey** (both public — this is
the "network identity" half of step 2's exchange, now that you have an operator key too).

### 4. The other side: self-claim, no operator credentials needed

Log into [bunsenbrenner.org/portal](https://bunsenbrenner.org/portal) with the email that just
got allow-listed. The channel shows up automatically on **Your Channels**, pending.

Compute your own claim material — same command as step 3's derivation, but from your side:

```bash
CT_CHANNEL_OPERATOR_PUBKEY=<operator's operator_pubkey, just received> \
CT_CHANNEL_BRIDGE_HOLDER=<operator's holder_pubkey, just received> \
CT_CHANNEL_HOLDER_KEY=<your own private holder key, from step 1> \
CT_CHANNEL_NOISE_PUBKEY=<your own noise_pubkey, from step 1> \
ct-agent channel member-material
```

The `channel_id` this prints will match the operator's exactly — that's the deterministic
derivation proving itself. Submit the claim on the channel's page (`holder_pubkey`,
`noise_pubkey`, `noise_attestation` from the output above). No operator token, no admin token,
no channel id to be told — the portal already knows which channel from the URL.

### 5. The operator: mint both grants

Self-claiming adds a membership row, but — same reason there's no member-list API — it doesn't
hand the operator a grant to mint automatically. Since both holder_pubkeys were already
exchanged in step 2/3, the operator mints both directly:

```bash
EXPIRES=$(( $(date +%s) + 31536000 ))  # 1 year; re-run to rotate

CT_CHANNEL_OPERATOR_KEY=<your operator private key> \
CT_GRANT_CHANNEL=<channel_id> CT_GRANT_MEMBER_HOLDER=<your own holder_pubkey> \
CT_GRANT_DIRECTION=<initiate|accept, matching your own role> CT_GRANT_EXPIRES=$EXPIRES \
ct-agent channel grant   # your own grant

CT_CHANNEL_OPERATOR_KEY=<your operator private key> \
CT_GRANT_CHANNEL=<channel_id> CT_GRANT_MEMBER_HOLDER=<the other side's holder_pubkey> \
CT_GRANT_DIRECTION=<the opposite of yours> CT_GRANT_EXPIRES=$EXPIRES \
ct-agent channel grant   # their grant — send this hex string back to them
```

`CT_GRANT_DIRECTION` has to match who dials whom: whoever runs a long-lived
`ct-agent channel --serve`-style listener is `accept`; whoever connects to reach them is
`initiate`. Getting it backwards isn't a security problem, just a "wrong direction" admission
failure that costs a re-mint.

### 6. Both sides: connect

```bash
CT_CHANNEL_ROLE=<initiate|accept> \
CT_CHANNEL_BROKER=edge:4435 CT_CHANNEL_RELAY=edge:4436 CT_CHANNEL_RELAY_ONLY=1 \
CT_CHANNEL_HOLDER_KEY=<your private holder key> CT_CHANNEL_NOISE_KEY=<your private noise key> \
CT_CHANNEL_GRANT=<your grant hex, from step 5> \
ct-agent channel
```

(`CT_CHANNEL_RELAY_ONLY=1` is the safe default behind NAT — bob-1's situation exactly. Drop it,
and set `CT_CHANNEL_LISTEN`, if you have a reachable address and want the direct path.)

## Proof this actually works

Live-verified 2026-08-01 against production, with two throwaway portal accounts standing in for
two independent participants — real Ed25519/X25519 identities, real deterministic channel-id
agreement (both sides independently derived the identical channel id from nothing but the
exchanged public keys), a real self-service claim through the actual portal HTML form endpoint,
real operator-signed grants, and a real message crossing the real edge relay in **both**
directions:

```
# initiator (member A) sent "hello-from-member-a" via stdin, received:
hello-from-member-b

# acceptor (member B) sent "hello-from-member-b" via stdin, received:
hello-from-member-a
```

No step in this document used anything beyond what a real participant has: their own generated
keys, a portal account, and the two public values exchanged with their peer.

## Related

- [README.md](README.md)'s "Provisioning the real channel" section — the demo-operator
  alternative to this page, for whoever runs this repo's own `compose.a2a-demo.yml`.
- [Set up an Agent-Fabric channel](https://docs.bunsenbrenner.org/how-to/join-a-channel/) and
  [Self-serve a channel membership grant](https://docs.bunsenbrenner.org/how-to/self-service-channel-grant/)
  on docs.bunsenbrenner.org — the general (not demo-specific) versions of steps 1–5 above.
- [Recover a channel when the operator key is lost](https://docs.bunsenbrenner.org/how-to/recover-lost-channel-operator-key/) —
  what to do if whichever of you holds `CT_CHANNEL_OPERATOR_KEY` loses it later.
