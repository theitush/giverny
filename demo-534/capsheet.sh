#!/bin/bash
# capsheet.sh <name> <frame>...: composed frames (with their caption band) into one grid; path printed.
SEQ='C:\Users\ita\AppData\Local\Temp\g534\seq'; name=$1; shift
F=$(for n in "$@"; do printf '%s\\%06d.jpg,' "$SEQ" $n; done); F=${F%,}; L=$(IFS=,; echo "$*")
out="C:\\Users\\ita\\AppData\\Local\\Temp\\g534\\cap-$name.jpg"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w /home/ita/giverny/.claude/review/534/rig/sheet.ps1)" -Files "$F" -Labels "$L" -Out "$out" -Cols 2 -CW 960 -CX 0 -CY 0 -SW 1280 -SH 884 >/dev/null 2>&1
echo /mnt/c/Users/ita/AppData/Local/Temp/g534/cap-$name.jpg
