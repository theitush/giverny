# lib.sh: sourced by each chunk of the giverny#534 take. The timeline (TSV: kind, epoch ms, arg) drives compose.ps1:
# start/end, cap "<tags>|<text>", speed <x>, cut/uncut. Coordinates are the window content's, in points.
R=/home/ita/giverny/.claude/review/534/rig; TL=$R/take/timeline.tsv
WT=/mnt/c/Users/ita/AppData/Local/Temp/g534/take
M=$(cut -d' ' -f1 $R/ids.txt); H=$(cut -d' ' -f2 $R/ids.txt)
ev() { printf '%s\t%s\t%s\n' "$1" "$(date +%s%3N)" "$2" >> "$TL"; }
cap() { ev cap "$1"; }
speed() { ev speed "$1"; }
d() { "$R/drive.sh" "$@"; }
g() { "$R/dbg.sh" "$@"; }
w() { local n=$(( ${1%.*} * 4 )); for _ in $(seq 1 $n); do sleep 0.25; done; }
show() { GIVERNY_FEED_DIR=$R/live/feeds "$R/giverny-debug" manage show --session "${1:-$M}" 2>&1; }
# wait_for '<regex>' <timeout s> [session]: until `manage show` matches.
wait_for() { local end=$(( $(date +%s) + $2 )); while [ $(date +%s) -lt $end ]; do show "${3:-$M}" | grep -qE "$1" && return 0; sleep 1; done; echo "timeout: $1"; return 1; }
mark() { ev mark "$1"; }
# until '<regex>' <timeout s>: approve demo prompts while waiting for `manage show` to match.
until_() { local end=$(( $(date +%s) + $2 )); while [ $(date +%s) -lt $end ]; do show | grep -qE "$1" && return 0; "$R/approve.sh" | grep -v '^none' ; sleep 1.5; done; echo "timeout: $1"; return 1; }
