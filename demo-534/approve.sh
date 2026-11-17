#!/bin/bash
# approve.sh: if the active tab shows a Claude Code permission prompt for a command of this demo's
# (giverny-manage, echo, sleep, sed -n, cat, python3 greet.py/bench.py, ls, a docs/ file edit), press Enter (1. Yes).
# Prints what it did. Anything else is left for a person.
R=/home/ita/giverny/.claude/review/534/rig; f=$R/live/screen.txt; rm -f $f
$R/dbg.sh "dump $f" >/dev/null; for _ in $(seq 1 20); do [ -s $f ] && break; sleep 0.1; done
grep -q 'Do you want to' $f || { echo none; exit 0; }
body=$(awk '/Do you want to/{exit} {print}' $f | tail -25)
if echo "$body" | grep -qE 'rm |kill|curl|wget|sudo|git push|chmod|TaskStop'; then echo "LEFT: $(echo "$body" | grep -vE '^\s*$' | tail -4 | tr '\n' ' ')"; exit 1; fi
$R/drive.sh 'type \r'; echo "approved: $(echo "$body" | grep -vE '^\s*$' | tail -3 | tr '\n' ' ' | cut -c1-160)"
