# Builds two binaries into one image: the real `ct-agent` (from its own standalone repo,
# used to spawn agent-bob's own process each round -- see README) and this repo's
# `a2a-demo-bridge`. agent-alice runs in a completely separate container/image
# (Alice.Dockerfile) -- this image never touches her handler or her keys. Matching-
# base-images discipline throughout (avoids the GLIBC cross-stage drift bug found in
# ct-agent's own docker/Dockerfile earlier this session).

FROM rust:1-slim-bookworm AS ct-agent-builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
# Same pin as this ecosystem's other Agent.Dockerfiles this session (CADS-auction-demo) --
# past v0.3.0, no tag cut yet that includes the fixes landed since.
ARG CT_AGENT_REF=1305b4eaf94bb36ad9a4c57d420135eb60e19bd0
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked \
    && cp target/release/ct-agent /tmp/ct-agent

FROM rust:1-slim-bookworm AS bridge-builder
WORKDIR /work
COPY Cargo.toml Cargo.lock* ./
COPY src src
COPY index.html index.html
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target \
    cargo build --release \
    && cp target/release/a2a-demo-bridge /tmp/a2a-demo-bridge

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=ct-agent-builder /tmp/ct-agent /usr/local/bin/ct-agent
COPY --from=bridge-builder /tmp/a2a-demo-bridge /usr/local/bin/a2a-demo-bridge
ENV A2A_BRIDGE_LISTEN=0.0.0.0:8790
ENV CT_AGENT_BIN=/usr/local/bin/ct-agent
EXPOSE 8790
ENTRYPOINT ["/usr/local/bin/a2a-demo-bridge"]
