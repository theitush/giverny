#!/bin/bash
# See the band off screen. No tokens are spent unless LOOK_PROMPT sends a prompt.
#
#   dev/look.sh <fixture.json> [width]        e.g. dev/look.sh dev/fixtures/busy.json 170
#
# Starts `claude --plugin-dir manager-mod` in a private tmux server (his `tui: fullscreen` holds
# there) with the fixture as this session's feed, prints `tmux capture-pane -e -p` (colours as
# escapes) to stdout and the layout to stderr, then kills the server, clears the feed and says so
# if ~/.claude/settings.json changed.
# A time in a fixture may be `"@-14m"`: that long before now.
#
#   LOOK_KEYS="C-x Tab Enter"   tmux send-keys words, one at a time, before the capture
#   LOOK_HEIGHT=48              the terminal's rows
#   LOOK_ARGS="--flag value"    more claude arguments (no spaces inside one)
#   LOOK_SETTINGS='{...}'       passed as --settings; by default the plugin's own settings.json,
#                               since a subagentStatusLine in ~/.claude/settings.json beats a plugin's
#   LOOK_PLUGIN=<dir>           another copy of the plugin
#   LOOK_PROMPT="..."           typed and sent, which spends tokens: for a live worker, on haiku
#   LOOK_WAIT=30                seconds to wait after the prompt
set -u
here=$(cd "$(dirname "$0")" && pwd)
mod=${LOOK_PLUGIN:-$(dirname "$here")}
fixture=${1:?usage: look.sh <fixture.json> [width]}
width=${2:-170}
sock=mgr-look-$$
id=$(cat /proc/sys/kernel/random/uuid)
feed=${XDG_CONFIG_HOME:-$HOME/.config}/giverny/feeds/$id.json
before=$(sha256sum "$HOME/.claude/settings.json")

cleanup() {
  tmux -L "$sock" kill-server 2>/dev/null
  giverny-manage clear --session "$id" >/dev/null 2>&1
  rm -f "$feed"
  rm -rf "$HOME/.claude/projects/-home-ita-giverny/$id" "$HOME/.claude/projects/-home-ita-giverny/$id.jsonl"
  [ "$before" = "$(sha256sum "$HOME/.claude/settings.json")" ] || echo "look.sh: ~/.claude/settings.json CHANGED" >&2
}
trap cleanup EXIT

mkdir -p "$(dirname "$feed")"
body=$(cat "$fixture")
for ago in $(grep -o '"@-[0-9]*m"' "$fixture" | tr -d '"' | sort -u); do
  body=${body//\"$ago\"/\"$(date -u -d "${ago#@}in" +%FT%TZ)\"}
done
printf '%s\n' "$body" >"$feed.tmp" && mv "$feed.tmp" "$feed"

cmd=(env -u GIVERNY_TAB_ID -u GIVERNY_PID -u GIVERNY_NONCE -u GIVERNY_CATEGORY -u GIVERNY_FEED_DIR -u CLAUDE_CODE_SESSION_ID
  claude --model haiku --session-id "$id" --plugin-dir "$mod")
cmd+=(--settings "${LOOK_SETTINGS:-$(cat "$mod/settings.json")}")
# shellcheck disable=SC2206
cmd+=(${LOOK_ARGS:-})
tmux -L "$sock" -f /dev/null new-session -d -x "$width" -y "${LOOK_HEIGHT:-48}" -c /home/ita/giverny "$(printf '%q ' "${cmd[@]}")"

for _ in $(seq 60); do
  tmux -L "$sock" capture-pane -p 2>/dev/null | grep -q '❯' && break
  sleep 0.5
done
# The prompt comes before the plugin has loaded: wait for the band, or as long as it could take.
for _ in $(seq 24); do
  tmux -L "$sock" capture-pane -p | grep -q 'Manager' && break
  sleep 0.5
done
sleep 1
if [ -n "${LOOK_PROMPT:-}" ]; then
  tmux -L "$sock" send-keys -l "$LOOK_PROMPT"
  sleep 0.5
  tmux -L "$sock" send-keys Enter
  sleep "${LOOK_WAIT:-30}"
fi
for key in ${LOOK_KEYS:-}; do
  tmux -L "$sock" send-keys "$key"
  sleep 0.5
done
[ -n "${LOOK_KEYS:-}" ] && sleep 1
echo "look.sh: session $id, ${width} columns, alternate screen (fullscreen): $(tmux -L "$sock" display -p '#{alternate_on}')" >&2
tmux -L "$sock" capture-pane -e -p
