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
ARG CT_AGENT_REF=6cb662d
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
CMD ["ct-agent"]
