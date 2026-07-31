#!/bin/sh
# The responder's real answer logic -- read one line from stdin, reply on stdout.
# Same shape as docs.bunsenbrenner.org's verified _tutorials/first-channel.md handler,
# just slightly more visibly "doing something" than a bare echo: reverses the word
# order and stamps the reply with this container's own clock, so a viewer can tell the
# reply really came from a distinct process (its own PID, its own timestamp), not a
# canned string baked into the dashboard.
# No `set -e`: `ct-agent` feeds the caller's request to this script's stdin without a
# guaranteed trailing newline, so a POSIX `read -r` that hits EOF right after the last
# byte returns non-zero even though it captured the value correctly -- confirmed live
# (ct-agent channel logged "service handler exited exit status: 1" with this line
# under `set -eu`, despite the same script working fine when tested by hand piping
# through `echo`, which always adds the newline `read` was relying on). Matches
# docs.bunsenbrenner.org's own verified _tutorials/first-channel.md handler exactly.
read -r INPUT
REVERSED=$(printf '%s\n' "$INPUT" | awk '{for(i=NF;i>0;i--) printf "%s ", $i; print ""}' | sed 's/ *$//')
printf 'agent-alice (pid %s, %s): you said "%s" -- reversed: "%s"\n' "$$" "$(date -u +%H:%M:%S)" "$INPUT" "$REVERSED"
