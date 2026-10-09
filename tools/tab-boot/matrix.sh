#!/usr/bin/env bash
# The giverny#265 matrix: every scenario once per round, rounds interleaved so
# load swings spread over all of them. One JSON line per run on stdout.
#
#   matrix.sh ROUNDS >> runs.jsonl
#
# REPO is where tabs open. TYPICAL/BIG/HUGE name finished sessions to resume,
# each with a _NEEDLE (words of its last reply) and HUGE with HUGE_DIR. A
# resume appends Claude Code's own `last-prompt`/`cost-state` lines to the
# transcript, and a `/resume` typed in a warm claude lands in
# ~/.claude/history.jsonl, as both would by hand.
set -u
here=$(dirname "$(readlink -f "$0")")
tb() { python3 "$here/tab_boot.py" "$@"; }
rounds=${1:-5}
warm_rounds=${WARM_ROUNDS:-$rounds}
G=${REPO:?the repo to open tabs in}
TYPICAL=${TYPICAL:?a finished session id}
TYPICAL_NEEDLE=${TYPICAL_NEEDLE:?words of its last reply}
BIG=${BIG:?a finished session id}
BIG_NEEDLE=${BIG_NEEDLE:?words of its last reply}
HUGE=${HUGE:?a finished session id}
HUGE_DIR=${HUGE_DIR:?its repo}
HUGE_NEEDLE=${HUGE_NEEDLE:?words of its last reply}
NOPLUG='{"enabledPlugins":{"giverny@giverny":false,"frontend-design@claude-plugins-official":false}}'
NOHOOK='{"disableAllHooks":true}'
ALLOFF='{"disableAllHooks":true,"enabledPlugins":{"giverny@giverny":false,"frontend-design@claude-plugins-official":false}}'

for r in $(seq 1 "$rounds"); do
  tb shell --cwd $G --label shell
  tb cold --cwd $G --profile --label cold-new
  tb warmterm --cwd $G --profile --label warmterm-new
  tb cold --cwd $G --profile --resume-id $TYPICAL --needle "$TYPICAL_NEEDLE" --label cold-resume-typical
  tb cold --cwd $G --profile --resume-id $BIG --needle "$BIG_NEEDLE" --label cold-resume-big
  tb cold --cwd $HUGE_DIR --profile --resume-id $HUGE --needle "$HUGE_NEEDLE" --label cold-resume-huge
  tb warmterm --cwd $G --profile --resume-id $TYPICAL --needle "$TYPICAL_NEEDLE" --label warmterm-resume-typical
  tb warmterm --cwd $G --profile --resume-id $BIG --needle "$BIG_NEEDLE" --label warmterm-resume-big
  tb warmterm --cwd $G --profile --env ENABLE_CLAUDEAI_MCP_SERVERS=false --label trim-no-connectors
  tb warmterm --cwd $G --profile --label trim-strict-mcp -- --strict-mcp-config
  tb warmterm --cwd $G --profile --label trim-no-plugins -- --settings "$NOPLUG"
  tb warmterm --cwd $G --profile --label trim-no-hooks -- --settings "$NOHOOK"
  tb warmterm --cwd $G --profile --env ENABLE_CLAUDEAI_MCP_SERVERS=false --label trim-all -- --strict-mcp-config --settings "$ALLOFF"
  if [ "$r" -le "$warm_rounds" ]; then
    tb warmclaude --cwd $G --settle 8 --resume-id $TYPICAL --needle "$TYPICAL_NEEDLE" --label warmclaude-resume-typical
    tb warmclaude --cwd $G --settle 8 --resume-id $BIG --needle "$BIG_NEEDLE" --label warmclaude-resume-big
  fi
done
