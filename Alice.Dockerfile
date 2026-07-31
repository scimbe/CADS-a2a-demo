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
ARG CT_AGENT_REF=3a53877407cd4f72b9afa32b748549297f43732b
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked \
    && cp target/release/ct-agent /tmp/ct-agent

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/ct-agent /usr/local/bin/ct-agent
COPY handler-alice.sh /usr/local/bin/handler-alice.sh
RUN chmod +x /usr/local/bin/handler-alice.sh
ENV CT_AGENT_SERVICE_HANDLER_CMD=/usr/local/bin/handler-alice.sh
ENV CT_AGENT_SERVICES=text_generation
ENV CT_CHANNEL_ROLE=accept
ENV CT_CHANNEL_SERVE=1
ENV CT_CHANNEL_RELAY_ONLY=1
CMD ["ct-agent", "channel"]
