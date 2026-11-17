#!/bin/bash
# shot.sh <name>: one frame of the demo window into $SHOTS/<name>.jpg (Windows temp), path printed.
P=$(wslpath -w /home/ita/giverny/.claude/review/534/rig/rec.ps1)
d="C:\\Users\\ita\\AppData\\Local\\Temp\\g534\\shot-$1"
rm -rf "/mnt/c/Users/ita/AppData/Local/Temp/g534/shot-$1"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$P" -Out "$d" -Width 1356 -MaxSeconds 1 -IntervalMs 1500 >/dev/null 2>&1
ls /mnt/c/Users/ita/AppData/Local/Temp/g534/shot-$1/*.jpg | head -1
