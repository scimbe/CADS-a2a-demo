#!/usr/bin/env bash
# Shared helpers for the Nordwind scripts. Source, do not execute.
# Token sources, first match wins:
#   1. CT_OIDC_TOKEN                      a bearer token you already have (e.g. from `ct-agent login`)
#   2. NW_CP_CLIENT_ID + NW_CP_CLIENT_SECRET + NW_OIDC_ISSUER   service-account client credentials
# The script never asks for, stores or prints a password.
set -euo pipefail

NW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="${NW_GEN_DIR:-$NW_DIR/deploy/generated}"
CT_AGENT_BIN="${CT_AGENT_BIN:-ct-agent}"
NW_CP_URL="${NW_CP_URL:-https://bunsenbrenner.org}"

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "missing tool: $1"; }

nw_token() {
  if [ -n "${CT_OIDC_TOKEN:-}" ]; then printf '%s' "$CT_OIDC_TOKEN"; return; fi
  [ -n "${NW_CP_CLIENT_ID:-}" ] && [ -n "${NW_CP_CLIENT_SECRET:-}" ] && [ -n "${NW_OIDC_ISSUER:-}" ] \
    || die "no credentials: set CT_OIDC_TOKEN, or NW_CP_CLIENT_ID + NW_CP_CLIENT_SECRET + NW_OIDC_ISSUER"
  curl -fsS -X POST "${NW_OIDC_ISSUER%/}/protocol/openid-connect/token" \
    -d grant_type=client_credentials -d "client_id=$NW_CP_CLIENT_ID" --data-urlencode "client_secret=$NW_CP_CLIENT_SECRET" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"], end="")'
}

# cp METHOD PATH [JSON]  -> prints body, exits non-zero on HTTP error. 409 is returned as code 9.
cp_call() {
  local method="$1" path="$2" body="${3:-}" out code
  out="$(mktemp)"
  if [ -n "$body" ]; then
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$NW_CP_URL$path" -H "Authorization: Bearer $NW_TOKEN" -H 'content-type: application/json' -d "$body")
  else
    code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" "$NW_CP_URL$path" -H "Authorization: Bearer $NW_TOKEN")
  fi
  cat "$out"; rm -f "$out"
  case "$code" in 2*) return 0 ;; 409) return 9 ;; *) echo "HTTP $code for $method $path" >&2; return 1 ;; esac
}

# envval FILE KEY -> value of `export KEY=value` / `KEY=value`
envval() { grep -E "^(export )?$2=" "$1" | head -1 | sed -E 's/^[^=]*=//'; }
