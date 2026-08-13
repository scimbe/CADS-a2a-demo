# The PUBLIC-facing Browser-Plane agent that tunnels the dashboard to a real
# bunsenbrenner.org subdomain -- NOT the ct-agent binary the bridge spawns internally
# for the actual A2A channel demo (that one is built into Dockerfile/the main image).
# Same standalone-repo shape as CADS-auction-demo's/help-site's Agent.Dockerfile.

FROM rust:1-slim-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
# #248 debug (2026-08-01): same bump as this repo's main Dockerfile -- see its comment
# for why (attestation-format skew fixed in ct-agent's 6894a8a, then the RFC1918
# direct-upgrade-candidate fix in 883e20f). This binary is browser-plane only and never
# exercises the #104 channel-upgrade path, but is kept pinned in sync for consistency.
#
# Bumped 2026-08-13 to v0.4.8 (3823343f, ADMISSION_EXCHANGE_TIMEOUT 15s -> 45s -- the
# actual root cause of CADS-Tunnel#494, pinned by the operator via live edge logs).
# Confirmed a strict descendant of 72394eb/6894a8a/883e20f/fda4f4d (git merge-base
# --is-ancestor checked against all four before bumping) -- every #248 fix above is
# still included, this only adds the admission-timeout fix (#140) and the #16 TCP
# dial-fallback fix on top. Keep in sync with Alice.Dockerfile/Dockerfile/
# compose.a2a-demo.yml/compose.a2a-demo.selfservice.override.yml's own CT_AGENT_REF.
ARG CT_AGENT_REF=3823343fdc47ea4ed91819cb68bfa8e89399f3f8
RUN git clone https://github.com/scimbe/ct-agent.git /build && cd /build && git checkout "${CT_AGENT_REF}"
WORKDIR /build
RUN --mount=type=cache,id=cargo-registry-a2a-agent,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target-a2a-agent,target=/build/target \
    cargo build --release --locked --jobs 1 \
    && cp target/release/ct-agent /tmp/ct-agent

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/ct-agent /usr/local/bin/ct-agent
CMD ["ct-agent"]
