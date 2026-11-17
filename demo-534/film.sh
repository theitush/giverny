#!/bin/bash
# film.sh rec|start|stop: the recorder (PrintWindow frames into Windows temp) and the take's first and last instant.
. /home/ita/giverny/.claude/review/534/rig/lib.sh
case $1 in
rec)
  mkdir -p "$R/take"; : > "$TL"; rm -rf "$WT"; mkdir -p "$WT"
  P=$(wslpath -w "$R/rec.ps1")
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$P" -Out 'C:\Users\ita\AppData\Local\Temp\g534\take\raw' \
    -Width 1356 -Stop 'C:\Users\ita\AppData\Local\Temp\g534\take\stop' -IntervalMs 90 -MaxSeconds 2700 | tr -d '\r' | tail -2
  ;;
start) ev start; ev cut ;;
stop) ev end; touch "$WT/stop" ;;
esac
