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
# Bumped 2026-08-19 to v0.5.7 (CADS-Tunnel#587): the previous pin
# (3823343fdc47ea4ed91819cb68bfa8e89399f3f8) was captured 2026-08-13 but had silently
# drifted 77 commits/~6 days behind ct-agent main by the time this was caught -- this
# repo has its own separate pinning family, invisible to CADS-Tunnel's own
# every_ct_agent_pin_matches_the_release guard test (which only scans CADS-Tunnel's
# tree). Most importantly this pulls in ct-agent#35/#41 (Noise-key attestation now
# enforced on all three channel-session code paths -- was previously enforced on none
# for a raw-pinned commit this old). Using a real tag (not another bare commit) from
# here on, matching CADS-Tunnel's own relay-node.Dockerfile convention -- a tag is a
# CI-gated, version-checked release with published binaries. Keep in sync with
# Alice.Dockerfile/Dockerfile/compose.a2a-demo.yml/
# compose.a2a-demo.selfservice.override.yml's own CT_AGENT_REF.
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
