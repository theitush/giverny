#!/bin/bash
# film.sh <chunk>: one stretch of the giverny#533 take (no feed: only what the PRs do by themselves). Each chunk opens with `uncut` and ends with `cut`,
# so the pauses between chunks leave the video. Coordinates are the window content's, in points.
R=/home/ita/giverny/.claude/review/533/rig; TL=$R/take/timeline.tsv; D=$R/drive.sh
WT=/mnt/c/Users/ita/AppData/Local/Temp/g533/take
ev() { printf '%s\t%s\t%s\n' "$1" "$(date +%s%3N)" "$2" >> "$TL"; }
cap() { ev cap "$1"; }
speed() { ev speed "$1"; }
d() { "$D" "$@"; }
case $1 in
rec)
  mkdir -p "$R/take"; : > "$TL"; rm -rf "$WT"; mkdir -p "$WT"
  P=$(wslpath -w "$R/rec.ps1")
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$P" -Out 'C:\Users\ita\AppData\Local\Temp\g533\take\raw' \
    -Width 1356 -Stop 'C:\Users\ita\AppData\Local\Temp\g533\take\stop' -IntervalMs 60 -MaxSeconds 1500 | tr -d '\r' | tail -1
  ;;
start) ev start; ev cut ;;
stop) ev end; touch "$WT/stop" ;;
rail1) ev uncut
  cap "#36|Every tab's CPU and memory, read from /proc once a second, at the right of its row."
  sleep 1; d 'move 150 272 900'; sleep 3
  cap "#36|Under the account bars, the total for the app and everything it runs. Its hover says what it counts."
  d 'move 135 800 800'; sleep 4
  cap "#36|A collapsed group's header adds its tabs up."
  d 'click 75 486 700'; sleep 3.5
  cap "#37|A finish you haven't looked at: a tick when it went well, a cross when it failed (lint exited 1)."
  d 'move 40 315 800'; sleep 3.5
  cap "#37|The ACME header keeps the tick while any finish under it went well. Looking at the tab clears the cross."
  d 'move 70 236 500'; sleep 2.5; d 'click 150 315 600'; sleep 3.5
  ev cut ;;
rail2) ev uncut
  cap "#34|Hover a live tab: a pause button beside its x. One click ends what the tab runs, and keeps the tab."
  d 'move 150 401 700'; sleep 1.5; d 'move 229 396 400'; sleep 1.2; d 'click 229 396 100'; sleep 3
  cap "#34|Right-click a group: stop all. It asks first, and lists what would die."
  d 'rclick 60 486 600'; sleep 1.5; d 'click 90 544 400'; sleep 3; d 'click 539 444 500'; sleep 3
  ev cut ;;
away) ev uncut
  cap "#33|Stop the notes tab's Claude Code, then start the tab again: it resumes the same conversation."
  d 'click 150 444 600'; sleep 2; d 'move 229 439 400'; sleep 1; d 'click 229 439 100'; sleep 2.5
  d 'click 700 400 600'; sleep 9
  cap "#33|On its next step the session is told what happened while it was away: here, its Claude Code restarted."
  d "slow Did Giverny tell you anything just now? Quote it word for word.\r"; sleep 4; speed 3; sleep 9; speed 1; sleep 6
  ev cut ;;
jobs) ev uncut
  cap "#35|BACKGROUND lists Claude Code's background jobs. Hover one: its x runs claude stop."
  d 'move 150 80 800'; sleep 2; d 'click 247 77 300'; sleep 4
  cap "#35|Right-click: open, stop agent, or delete agent... (claude rm, behind a confirm)."
  d 'rclick 150 80 600'; sleep 1.5; d 'move 202 147 400'; sleep 1.5; d 'move 290 147 200' 'click 304 180 300'; sleep 3.5
  ev cut ;;
panel) ev uncut
  cap "#38|The claude in this tab (haiku, on a short script for the demo) starts two background agents."
  d 'click 150 94 700'; sleep 1.5; d "slow start the two workers\r"; sleep 1.2
  "$R/agents.py" wait notes exists 60; "$R/agents.py" wait changelog exists 30; sleep 2.5
  cap "#38|Each shows as Running within a second, read off Claude Code's own subagents/ files: time, what it is doing, tokens."
  sleep 5
  cap "#38|With the panel on, Claude Code's own agent list stays out of the picture, and its prompt doesn't move."
  sleep 4.5
  cap "#40|No Next up and no estimates: those come from a feed that a manager writes (docs/management-panel.md). These PRs ship none."
  sleep 5.5
  ev cut ;;
page) ev uncut
  cap "#39|Click a row: the worker's page, drawn from its transcript, following it live."
  d 'click 420 751 700'; sleep 5
  cap "#41|The page's prompt box sends a line to this worker."
  d "slowtext also echo today's date"; sleep 1.2; d 'key enter'; sleep 3
  cap "#41|The line shows on the page at once, dimmed, until the worker takes it at its next step."
  speed 3; sleep 12; speed 1; sleep 3
  ev cut ;;
mgr) ev uncut
  cap "#41|There is no manager in these PRs: the line went to the claude in the tab, as if typed there. It passed it on with SendMessage."
  d 'key esc'; sleep 6
  cap "#38|Turn the panel off, and Claude Code draws its own agent list again."
  d 'settings management panel'; sleep 1.5; d 'click 720 69 500'; sleep 1.2; d 'settings'; sleep 4.5
  cap "#38|Back on: the list goes, and the panel is back."
  d 'settings management panel'; sleep 1; d 'click 720 69 500'; sleep 1; d 'settings'; sleep 3.5
  ev cut ;;
land) ev uncut
  cap "#38|Waiting for the workers to land."
  speed 4; "$R/agents.py" wait changelog finished 240; "$R/agents.py" wait notes finished 240; sleep 3; speed 1
  cap "#38|Done once Claude Code's own task notification says so: the time each took and how it ended, newest first."
  sleep 5
  ev cut ;;
wake) ev uncut
  cap "#39|A Done row's page opens on the worker's report."
  d "click 420 $2 700"; sleep 5
  cap "#41|A line from a Done worker's page wakes it, and its row is Running again until it lands."
  t0=$(date +%s); d 'slowtext add one more line: credits to y0av'; sleep 1; d 'key enter'; sleep 3
  speed 3; "$R/agents.py" wait changelog finished 120 "$t0"; sleep 3; speed 1; sleep 3
  d 'key esc'; sleep 1.5
  cap "|The panel is off by default (claude.management_panel). Giverny writes nothing to Claude Code's settings for it."
  sleep 4.5
  ev cut ;;
esac
