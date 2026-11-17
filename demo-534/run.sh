#!/bin/bash
# giverny#534 demo window: task-534-demo-fork's debug build (agents-pane + the demo driver) in its own
# XDG, runtime, feed, ledger and history dirs, GIVERNY_NO_ACCOUNT_SETUP=1, its plugin written into its
# own config (GIVERNY_DEMO_PLUGIN) and loaded by each test claude with --plugin-dir. Test claudes run on
# project settings only, haiku, no MCP. Nothing here writes ~/.claude/settings.json or ~/.config/giverny.
# run.sh [keep]: keep = keep live/ (feeds, history) from the last window.
set -u
R=/home/ita/giverny/.claude/review/534/rig
L=$R/live; N=/home/ita/.nvm/versions/node/v22.22.1/bin
A=/home/ita/giverny/demo-534/acme   # outside any .claude path: Claude Code asks for every edit under one
bin=$R/giverny-debug
for f in /home/ita/giverny/.claude/review/*/run.pid /home/ita/giverny/.claude/review/*/rig/run.pid; do
  [ -f "$f" ] && [ -d /proc/$(cat "$f") ] && { echo "a test window is open ($f: $(cat $f)); close it first"; exit 5; }
done
free=$(df --output=avail -BG /mnt/c | tail -1 | tr -dc 0-9)
[ "$free" -lt 8 ] && { echo "C: has ${free}G free: under 8G, not opening"; exit 4; }
"$R/hashes.sh" save || exit 3
if [ "${1:-}" != keep ]; then rm -rf "$L"; fi
mkdir -p "$L"/{xdg/config/giverny/state,xdg/data,xdg/state,xdg/cache,bin,feeds/resources}
rt=$L/rt; rm -rf "$rt"; mkdir -m 700 "$rt"
# The app's socket is its own; systemd's are the real ones, so `run` and the tab cap meet the real user systemd.
ln -s /run/user/1000/systemd "$rt/systemd"; ln -s /run/user/1000/bus "$rt/bus"
P=$L/xdg/config/giverny/claude-plugin/plugins/giverny
cat > "$L/xdg/config/giverny/config.toml" <<X
[claude]
management_panel = true

[codex]
enabled = false

[update]
check = false

[rail]
place_titles = true

[management_panel]
manage_skill = true
ask_names = true

[management_panel.lease]
ram = "1G"

# The same as the real config: the slice is one per machine, and the tab cap and \`run\` re-apply these.
[manager.limits]
ram = "22G"
cpu_cores = 8
X
printf '{"version":"1.7.2"}' > "$L/xdg/config/giverny/state/welcome.json"
python3 - "$L" "$A" "$bin" <<'PY'
import json,sys
L,a,g=sys.argv[1:]
tab=lambda i,c,t:{"id":i,"category":c,"custom_title":t,"auto_title":"","cwd":a,"claude_session":None,"claude_config_dir":None,"foreground":None,"git_repo":None,"wsl_distro":None}
cat=lambda i,n,c:{"id":i,"name":n,"color_index":c,"collapsed":False,"profile_dir":None}
st={"version":1,"boot_id":open('/proc/sys/kernel/random/boot_id').read().strip(),"clean_shutdown":True,"font_size":11.0,
 "layout":{"window":[1280.0,820.0],"maximized":False,"rail_width":270.0,"rail_view":"categories","collapsed_repos":[],"zoom":None,"work_area":None,"settings_section":"management panel"},
 "workspace":{"next_id":4,"categories":[cat(1,"acme",1),cat(2,"infra",2)],
  "tabs":[tab(1,1,None),tab(2,1,"shell"),tab(3,2,None)],
  "active":1,"recent":[]}}
json.dump(st,open(L+"/xdg/config/giverny/state/tabs.json","w"),indent=1)
# What this build's installer writes for an account: its relay hooks and status line. The plugin comes by --plugin-dir.
ev=["SessionStart","UserPromptSubmit","PostToolUse","PreCompact","Stop","StopFailure","Notification","SessionEnd"]
hooks={e:[{"hooks":[{"type":"command","command":g+" relay","async":True,"timeout":10}]}] for e in ev}
allow=["Bash(giverny-manage:*)","Bash(giverny-eta:*)","Bash(sleep:*)","Bash(echo:*)","Bash(date:*)","Bash(cat:*)",
       "Bash(ls:*)","Bash(python3 greet.py:*)","Bash(nohup true)","Read","Glob","Grep",
       "Edit(//home/ita/giverny/demo-534/acme/**)","Write(//home/ita/giverny/demo-534/acme/**)"]
st={"tui":"fullscreen","hooks":hooks,"statusLine":{"type":"command","command":g+" statusline","padding":0,"refreshInterval":30},
    "permissions":{"allow":allow,"defaultMode":"acceptEdits"},
    "claudeMdExcludes":["/home/ita/giverny/**","/home/ita/.claude/CLAUDE.md","/home/ita/CLAUDE.md"]}
json.dump(st,open(L+"/tab-settings.json","w"),indent=2)
PY
echo '{"mcpServers":{}}' > "$L/no-mcp.json"
cat > "$L/bin/claude" <<X
#!/bin/sh
# No MCP at all: no claude.ai connector loads (checked: the tool list has no mcp__ tools).
export ENABLE_CLAUDEAI_MCP_SERVERS=false
exec $N/claude --setting-sources project --settings $L/tab-settings.json --plugin-dir $P \\
  --strict-mcp-config --mcp-config $L/no-mcp.json --model haiku "\$@"
X
cat > "$L/bin/giverny-manage" <<X
#!/bin/sh
exec $bin manage "\$@"
X
chmod +x "$L/bin/claude" "$L/bin/giverny-manage"
cat > "$L/demo-rc" <<'X'
if [ -z "${__demo_rc-}" ]; then __demo_rc=1; PS1='\[\e[1;34m\]\W\[\e[0m\] \$ '; fi
X
cd "$A"
setsid env XDG_RUNTIME_DIR=/run/user/1000 systemd-run --user --scope --quiet --collect --unit="demo-534-$$" env -i \
  PATH="$L/bin:$N:/usr/local/bin:/usr/bin:/bin" HOME="$HOME" USER="$USER" SHELL=/bin/bash TERM=xterm-256color LANG=C.UTF-8 \
  PROMPT_COMMAND=". $L/demo-rc" \
  DISPLAY=:0 GALLIUM_DRIVER=d3d12 XDG_RUNTIME_DIR="$rt" DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  XDG_CONFIG_HOME="$L/xdg/config" XDG_DATA_HOME="$L/xdg/data" XDG_STATE_HOME="$L/xdg/state" XDG_CACHE_HOME="$L/xdg/cache" \
  GIVERNY_FEED_DIR="$L/feeds" GIVERNY_LEDGER="$L/feeds/resources/ledger.json" GIVERNY_MANAGE_HISTORY="$L/feeds/history.jsonl" \
  GIVERNY_NO_ACCOUNT_SETUP=1 GIVERNY_DEMO_PLUGIN=1 GIVERNY_DEMO_ACCOUNT=you \
  GIVERNY_DEMO_CMD="$L/cmd" GIVERNY_DEBUG_CMD="$L/dbg" CLAUDE_CODE_SUBAGENT_MODEL=haiku RUST_LOG=giverny=info \
  "$bin" >> "$L/giverny.log" 2>&1 < /dev/null &
for _ in $(seq 1 40); do p=$(pgrep -f -x "$bin" | head -1); [ -n "$p" ] && break; sleep 0.25; done
echo "$p" > "$R/run.pid"; echo "window pid $p"
