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
ARG CT_AGENT_REF=fb5a799
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
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
