#!/bin/bash
# drive.sh '<cmd>' ['<cmd>'...]: hand commands to the window's GIVERNY_DEBUG_CMD file in one take, then wait until it is taken.
C=/home/ita/giverny/.claude/review/533/rig/live/cmd
for _ in $(seq 1 50); do [ -e "$C" ] || break; sleep 0.1; done
printf '%s\n' "$@" > "$C.tmp" && mv "$C.tmp" "$C"
for _ in $(seq 1 50); do [ -e "$C" ] || exit 0; sleep 0.1; done; echo "not taken"; exit 1
