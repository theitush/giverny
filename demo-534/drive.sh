#!/bin/bash
# drive.sh '<cmd>' ...: the demo driver (GIVERNY_DEMO_CMD: pointer, typing). dbg.sh: the branch's own (GIVERNY_DEBUG_CMD).
C=/home/ita/giverny/.claude/review/534/rig/live/${DRIVE_FILE:-cmd}
for _ in $(seq 1 50); do [ -e "$C" ] || break; sleep 0.1; done
printf '%s\n' "$@" > "$C.tmp" && mv "$C.tmp" "$C"
for _ in $(seq 1 50); do [ -e "$C" ] || exit 0; sleep 0.1; done; echo "not taken"; exit 1
