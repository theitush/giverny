#!/bin/bash
# sheet.sh <name> <HH:MM:SS>...: the raw frames nearest those times, as one grid in Windows temp; path printed.
RAW=/mnt/c/Users/ita/AppData/Local/Temp/g534/take/raw; name=$1; shift
files=(); labs=()
list=$(ls $RAW)
for t in "$@"; do ms=$(date -d "${DAY:-2026-11-17} $t" +%s%3N); f=$(echo "$list" | awk -v m=$ms -F. '$1>=m{print; exit}'); files+=("$(wslpath -w $RAW/$f)"); labs+=("$t"); done
F=$(IFS=,; echo "${files[*]}"); L=$(IFS=,; echo "${labs[*]}")
out="C:\\Users\\ita\\AppData\\Local\\Temp\\g534\\sheet-$name.jpg"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(wslpath -w /home/ita/giverny/.claude/review/534/rig/sheet.ps1)" -Files "$F" -Labels "$L" -Out "$out" ${COLS:+-Cols $COLS} >/dev/null 2>&1
echo /mnt/c/Users/ita/AppData/Local/Temp/g534/sheet-$name.jpg
