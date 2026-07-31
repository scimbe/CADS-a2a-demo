#!/bin/sh
# agent-alice's real answer logic, running inside agent-alice's own container --
# never spawned by the bridge, never sharing a filesystem with agent-bob. Same
# no-`set -e` reasoning as the bridge's own former handler.sh: ct-agent feeds the
# caller's request to this script's stdin without a guaranteed trailing newline, so a
# POSIX `read -r` hitting EOF right after the last byte returns non-zero even though it
# captured the value correctly.
read -r INPUT
REVERSED=$(printf '%s\n' "$INPUT" | awk '{for(i=NF;i>0;i--) printf "%s ", $i; print ""}' | sed 's/ *$//')
printf 'agent-alice (pid %s, %s): you said "%s" -- reversed: "%s"\n' "$$" "$(date -u +%H:%M:%S)" "$INPUT" "$REVERSED"
