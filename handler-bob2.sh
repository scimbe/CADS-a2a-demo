#!/bin/sh
# bob-2 ("remote")'s answer logic for the CADS-Tunnel#248 3-way A2A demo.
# ct-agent feeds the caller's request to this script's stdin without a guaranteed
# trailing newline, so a POSIX `read -r` hitting EOF right after the last byte
# returns non-zero even though it captured the value correctly -- no `set -e`.
read -r INPUT
REVERSED=$(printf '%s\n' "$INPUT" | awk '{for(i=NF;i>0;i--) printf "%s ", $i; print ""}' | sed 's/ *$//')
printf 'bob-2 (pid %s, %s): you said "%s" -- reversed: "%s"\n' "$$" "$(date -u +%H:%M:%S)" "$INPUT" "$REVERSED"
