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
# #248 debug (2026-08-01): the previous pin (1305b4e, CADS-Tunnel v0.4.1) predates
# ct-agent's own 6894a8a fix ("bump CADS-Tunnel pin from v0.4.1 to v0.4.8 -- breaking
# attestation-format skew", #252's length-prefixed preimage domain). A real ct-agent
# built from the old pin computes a DIFFERENT noise-attestation preimage than the live
# control-plane (v0.4.9+) expects -- deterministic "peer Noise-key attestation failed"
# against any peer whose attestation was registered under the new format, which is
# exactly the symptom that made alice-bob1/alice-bob2 fail nearly every round while the
# same-pin-built baseline "bob"/"alice" pair (self-consistently on the OLD format)
# always succeeded. Bumped past 6894a8a to ct-agent's current main tip, which pins
# CADS-Tunnel v0.4.9 (past the breaking change; no further attestation-format change
# since). No newer ct-agent tag exists yet (still v0.3.0), so pin the exact commit.
#
# 2026-08-01 (#248 continued): bumped again to 883e20f -- fixes the *next* bug found
# live once the attestation skew was fixed: the initiator (this bridge, co-located
# with the edge on the same Docker host) offered its edge-observed RFC1918 Docker-
# bridge address as a #104 direct-upgrade candidate to real external peers, who
# correctly refused to dial it but left the initiator hanging the full session
# timeout instead of degrading to relay promptly. See ct-agent's own commit message.
#
# 2026-08-01 (#248 continued again): bumped to fda4f4d -- adds the always-on
# uptime/bytes-sent/bytes-recvd status line (unconditional, no flag) and extends
# CT_DEBUG_A2A_TIMING with dial/accept/handshake-duration timing. Must move together
# with Alice.Dockerfile's pin (below) -- she's the responder every scenario dials, so a
# pin skew here means only one side of a session logs the new detail.
ARG CT_AGENT_REF=72394eb
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --jobs 1 \
    && cp target/release/ct-agent /tmp/ct-agent

FROM rust:1-slim-bookworm AS bridge-builder
WORKDIR /work
# This host has zero swap and has frozen before under concurrent heavy builds --
# BuildKit runs independent stages in parallel by default, which would run this
# stage's rustc alongside ct-agent-builder's. The COPY below is a no-op (the file
# is never used) whose only purpose is a data dependency that forces BuildKit to
# sequence the two compiles instead of running them concurrently.
COPY --from=ct-agent-builder /tmp/ct-agent /tmp/.ct-agent-builder-done
COPY Cargo.toml Cargo.lock* ./
COPY src src
COPY index.html index.html
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target \
    cargo build --release --jobs 1 \
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
