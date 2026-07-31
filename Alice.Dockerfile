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
ARG CT_AGENT_REF=3c180003ae240309a44103ff76339382d6518a58
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked \
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
CMD ["/usr/local/bin/alice-entrypoint.sh"]
