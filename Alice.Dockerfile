# agent-alice: a genuinely separate container from the bridge and from agent-bob.
# Runs the real `ct-agent` binary directly (`ct-agent channel`, broker-mediated,
# CT_CHANNEL_SERVE=1) as a long-lived process parked on the real edge's broker/relay,
# answering real service/text_generation calls via handler-alice.sh. No filesystem, no
# process tree, no Cargo workspace shared with the bridge or with agent-bob -- see
# compose.a2a-demo.yml for how this is wired to a real, operator-registered channel
# membership rather than a locally-pinned key pair.

FROM rust:1-slim-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
# #248 debug (2026-08-01): same bump as this repo's main Dockerfile -- MUST move
# together with the bridge's pin, not independently. Alice is the responder every
# scenario (bob/bob1/bob2) dials; if her attestation is registered under a different
# preimage format than whatever's verifying it, every scenario breaks, not just the
# ones this bump was meant to fix. See the main Dockerfile's comment for the root cause
# (ct-agent's 6894a8a, attestation-format skew).
#
# 2026-08-01 (#248 continued): bumped again to 883e20f alongside the main Dockerfile --
# see its comment. Alice is also a #104 initiator (she offers a candidate back to
# whichever bob dials her), so this fix applies to her role too, not just the bridge's.
#
# 2026-08-01 (#248 continued again): bumped to fda4f4d alongside the main Dockerfile --
# same always-on stats + extended CT_DEBUG_A2A_TIMING. Alice now runs host-native
# (systemd, see compose.a2a-demo.yml's migration comment), not from this image directly
# -- this pin documents what /home/becke/alice-host/ct-agent on the plane host should
# match, extracted the same way (docker build + docker cp) rather than run in place.
#
# Bumped 2026-08-19 to v0.5.7 (CADS-Tunnel#587): the previous pin
# (3823343fdc47ea4ed91819cb68bfa8e89399f3f8) had silently drifted 77 commits/~6 days
# behind ct-agent main -- pulls in ct-agent#35/#41 (Noise-key attestation now enforced
# on all three channel-session paths). Using a real tag from here on, matching
# CADS-Tunnel's own relay-node.Dockerfile convention. Must move together with
# Agent.Dockerfile/Dockerfile's own CT_AGENT_REF (same reasoning as every earlier
# bump above) -- NOTE: /home/becke/alice-host/'s actual running binary is a real
# host-native process with no systemd unit, shared with an unrelated CADS-devsystem
# github-issue-agent loop process; re-extracting into it is a separate, riskier step
# than this Dockerfile pin and is being tracked apart (see the #587 follow-up note),
# not assumed done just because this pin moved.
ARG CT_AGENT_REF=v0.7.22
# Optional gh-token secret (--secret id=gh_token,src=<file>): GitHub's anonymous
# git-clone rate limit for this host's IP was hit 2026-09-02 (same fix already
# applied to CADS-cookbook-demo/CADS-DEMO-deutschlandatlas-callcenter/
# CADS-webconference-demo). Falls back to a plain anonymous clone when no
# secret is passed, so this is a no-op for anyone building without a token.
RUN --mount=type=secret,id=gh_token \
    if [ -s /run/secrets/gh_token ]; then \
      git -c http.https://github.com/.extraheader="AUTHORIZATION: basic $(printf 'x:%s' "$(cat /run/secrets/gh_token)" | base64 -w0)" clone https://github.com/scimbe/ct-agent.git /build; \
    else \
      git clone https://github.com/scimbe/ct-agent.git /build; \
    fi \
    && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,id=cargo-registry-a2a-alice,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target-a2a-alice,target=/build/target \
    cargo build --release --locked --jobs 1 \
    && cp target/release/ct-agent /tmp/ct-agent

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xxd \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/ct-agent /usr/local/bin/ct-agent
COPY handler-alice.sh /usr/local/bin/handler-alice.sh
COPY alice-entrypoint.sh /usr/local/bin/alice-entrypoint.sh
RUN chmod +x /usr/local/bin/handler-alice.sh /usr/local/bin/alice-entrypoint.sh
ENV CT_AGENT_SERVICE_HANDLER_CMD=/usr/local/bin/handler-alice.sh
ENV CT_AGENT_SERVICES=text_generation
ENV CT_CHANNEL_ROLE=accept
ENV CT_CHANNEL_SERVE=1
# Deliberately relay-only: alice never binds/advertises a directly-dialable address, so
# this container never listens for inbound connections from the open internet -- the
# platform's outbound-only promise (see ct-agent's own package description) holds even
# for this containerized, publicly-reachable-host deployment. Data still flows only
# through the edge relay/broker; CT_CHANNEL_FRONT_DOOR{,_CERT} (alice-entrypoint.sh)
# only add a second TRANSPORT rung (TCP-:443) to REACH that relay, they do not open a
# peer-facing listener.
ENV CT_CHANNEL_RELAY_ONLY=1
# #104: opt in to the in-band relay->direct upgrade. Orthogonal to CT_CHANNEL_RELAY_ONLY
# above -- this never advertises or opens a new port, it only negotiates a candidate
# in-band, over the already-open, already-authenticated relay stream. On this specific
# deployment (alice, bob, and the edge all on one host) the edge-observed reflexive
# address is a private one, so the upgrade correctly refuses itself (SSRF guard) and the
# session stays on the relay -- but the real, tested wiring is genuinely live in
# production, ready for the day two members sit on genuinely separate networks.
ENV CT_CHANNEL_DIRECT_UPGRADE=1
CMD ["/usr/local/bin/alice-entrypoint.sh"]
