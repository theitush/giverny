#!/bin/bash
# take4.sh: the fresh take, event-driven in one run (the recorder already running). Short pointer glides,
# and nothing typed into the manager tab until its own prompt is back (a worker's view takes typed lines).
. /home/ita/giverny/.claude/review/534/rig/lib.sh
P=~/.claude/projects/-home-ita-giverny-demo-534-acme
spawned() { grep -l "\"description\":\"$1" $P/$M/subagents/agent-*.meta.json >/dev/null 2>&1; }
aw() { local end=$(( $(date +%s) + $1 )); while [ $(date +%s) -lt $end ]; do "$R/approve.sh" | grep -v '^none'; sleep 1; done; }
log() { echo "$(date +%T) $*"; }
screen() { local f=$R/live/screen.txt; rm -f $f; g "dump $f" >/dev/null; for _ in $(seq 1 20); do [ -s $f ] && break; sleep 0.1; done; cat $f 2>/dev/null; }
# The manager's own view back: the header's back button, then Esc, until no worker view is on screen.
main_view() { for i in 1 2 3 4; do if screen | grep -qE 'Message @|Message this worker'; then log "view up ($i)"; [ $i -le 2 ] && d 'click 352 579 250' || d 'key esc'; w 3; else g back >/dev/null 2>&1; return 0; fi; done; }
ev start; ev uncut
mark open; d 'move 700 420 300'; w 2
mark claude-start; d "type clear; claude --session-id $M --append-system-prompt-file $R/mgr-system.md\r"; w 6
mark hover-tab; d 'move 120 88 400'; w 4; d 'move 700 470 300'; w 2
mark ask; d 'slow manage the five tasks in .claude/plan.md\r'; w 5
mark planning; until_ 'deploy' 150; mark planned-all; log planned; g 'dragxy 762 739 762 590'
until_ 'running +server' 150; mark three-started; log started
end=$(( $(date +%s) + 90 )); while [ $(date +%s) -lt $end ] && ! spawned server; do "$R/approve.sh" | grep -v '^none'; sleep 1; done
mark workers-spawned; log spawned; aw 3
mark hover-docs; d 'move 560 603 250'; aw 3
mark hover-bench-use; d 'move 1180 622 250'; aw 4
mark hover-server; d 'move 940 641 250'; aw 4
mark hover-examples; d 'move 600 660 250'; aw 3
mark hover-deploy; d 'move 600 679 250'; aw 3
if show | grep -qE '^running +docs'; then
  mark click-docs; d 'click 560 603 250'; w 9; mark view-docs; w 5
  mark back; main_view; w 2
else mark docs-gone; fi
aw 2
mark shell; d 'click 112 130 250'; w 3
if screen | grep -q 'acme \$'; then
  d 'type giverny-manage resources\r'; w 7
  mark scopes; d 'type systemctl --user status "giverny-run-bench-*" --no-pager | head -8 | cut -c1-110\r'; w 6
  d 'type cat /proc/$(pgrep -f "session-id $CLAUDE_CODE_SESSION_ID" | head -1)/cgroup\r'; w 6
else log "shell not on screen"; fi
mark back-manager; d 'click 112 87 250'; w 3; main_view
mark waiting-landings; until_ '^done +docs' 300; mark docs-done; log docs-done
until_ '^done +server' 200; mark server-done
until_ '^done +bench' 300; mark bench-done; log bench-done
until_ '^done +examples' 300; mark examples-done; log examples-done
aw 4; main_view
mark unhold; d 'slow the docs review is done, go on with deploy\r'; w 6
until_ '^running +deploy' 150; mark deploy-running; log deploy-running
end=$(( $(date +%s) + 60 )); while [ $(date +%s) -lt $end ] && ! spawned deploy; do "$R/approve.sh" | grep -v '^none'; sleep 1; done; aw 4
until_ '^done +deploy' 240; mark deploy-done; log deploy-done; aw 6; main_view
mark clear-done; d 'slow /giverny:clear-done\r'; aw 10
mark settings; d 'settings management panel'; w 7; mark settings-titles; d 'settings tabs & titles'; w 6; d 'settings'; w 2
mark helper; d 'click 112 208 250'; w 3
d "type clear; claude --session-id $H --append-system-prompt-file $R/helper-system.md\r"; w 9
MP=$(pgrep -f -- "--session-id $M" | head -1)
mark kill; d "slow Run this exact command: kill -0 $MP\r"; aw 18
mark nohup; d 'slow Run this exact command: nohup true. Then quote word for word what Giverny told you about it.\r'; aw 20
touch $R/live/agent.mark
mark agent; d 'slow Start one background Explore agent, description "survey acme", prompt: list the files here, then run this one Bash call: echo a; sleep 20; echo b; sleep 20. Then do what Giverny asks you.\r'
end=$(( $(date +%s) + 90 )); while [ $(date +%s) -lt $end ] && [ -z "$(find $R/live/feeds/agent-etas -name '*.json' -newer $R/live/agent.mark 2>/dev/null)" ]; do "$R/approve.sh" | grep -v '^none'; sleep 1; done
mark agent-eta; log agent-eta; aw 12
mark resume; d 'slow /resume'; w 4; d 'type \r'; w 7; d 'key esc'; w 3
mark end; ev cut; ev end; log done; touch "$WT/stop"
