#!/bin/bash
# Close the window run.sh opened, only if its pid is still this rig's build; then check the hashes.
R=/home/ita/giverny/.claude/review/533/rig
p=$(cat "$R/run.pid" 2>/dev/null) && if tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null | grep -q "$R/giverny-debug"; then
  kill -TERM "$p"; for _ in $(seq 1 40); do [ -d /proc/$p ] || break; sleep 0.25; done
  [ -d /proc/$p ] && { echo "window $p still up"; exit 1; }
fi
rm -f "$R/run.pid"; echo closed; "$R/hashes.sh" check
