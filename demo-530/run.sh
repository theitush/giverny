#!/bin/bash
# giverny#530 demo window: the demo branch's debug build (y0av#33-#41 merged, plus the
# GIVERNY_DEBUG_CMD driver) in its own XDG and runtime dirs, with GIVERNY_NO_ACCOUNT_SETUP=1.
# Its claudes run on project settings only (a scratch settings file naming this build's hooks), haiku.
# Nothing here writes ~/.claude/settings.json or ~/.config/giverny. One window at a time.
set -u
R=/home/ita/giverny/.claude/review/530/rig
L=$R/live; N=/home/ita/.nvm/versions/node/v22.22.1/bin
W=/home/ita/giverny/.claude/review/522-stack/live/work   # already trusted: no new trust entry
A=/home/ita/giverny/.claude/acme   # under ~/giverny, which is trusted: no trust dialog, no new entry
bin=$R/giverny-debug
for f in /home/ita/giverny/.claude/review/*/run.pid /home/ita/giverny/.claude/review/*/rig/run.pid; do
  [ -f "$f" ] && [ -d /proc/$(cat "$f") ] && { echo "a test window is open ($f: $(cat $f)); close it first"; exit 5; }
done
free=$(df --output=avail -BG /mnt/c | tail -1 | tr -dc 0-9)
[ "$free" -lt 5 ] && { echo "C: has ${free}G free: under 5G, not opening"; exit 4; }
"$R/hashes.sh" save || exit 3
rm -rf "$L"; mkdir -p "$L"/{xdg/config/giverny/state,xdg/data,xdg/state,xdg/cache,bin,rt} "$A/src"; chmod 700 "$L/rt"
[ -d "$A/.git" ] || git -C "$A" init -q -b main
cp "$R/tests.py" "$R/db.py" "$R/queue.py" "$A/"; printf 'fn main() {}\n' > "$A/src/main.rs"; printf '[package]\nname = "acme"\n' > "$A/Cargo.toml"
cat > "$L/xdg/config/giverny/config.toml" <<X
[claude]
management_panel = true

[behavior]
history_per_tab = true

[codex]
enabled = false
X
printf '{"version":"1.7.3"}' > "$L/xdg/config/giverny/state/welcome.json"
python3 - "$L" "$W" "$A" "$bin" <<'PY'
import json,sys
L,w,a,g=sys.argv[1:]
tab=lambda i,c,t,d:{"id":i,"category":c,"custom_title":t,"auto_title":"","cwd":d,"claude_session":None,"claude_config_dir":None,"foreground":None,"git_repo":None,"wsl_distro":None}
cat=lambda i,n,c:{"id":i,"name":n,"color_index":c,"collapsed":False,"profile_dir":None}
st={"version":1,"boot_id":open('/proc/sys/kernel/random/boot_id').read().strip(),"clean_shutdown":True,"font_size":11.0,
 "layout":{"window":[1280.0,820.0],"maximized":False,"rail_width":270.0,"rail_view":"categories","collapsed_repos":[],"zoom":None,"work_area":None,"settings_section":"management panel"},
 "workspace":{"next_id":9,"categories":[cat(1,"manage",0),cat(2,"acme",1),cat(3,"infra",2)],
  "tabs":[tab(1,1,"manager",a),tab(2,2,"tests",a),tab(3,2,"lint",a),tab(4,2,"build",a),tab(5,2,"server",a),tab(6,2,"notes",a),tab(7,3,"db",a),tab(8,3,"queue",a)],
  "active":1,"recent":[]}}
json.dump(st,open(L+"/xdg/config/giverny/state/tabs.json","w"),indent=1)
# What this build's installer writes for an account: its hooks and status line, nothing for the panel.
ev=["SessionStart","UserPromptSubmit","PostToolUse","PreCompact","Stop","StopFailure","Notification","SessionEnd"]
hooks={e:[{"hooks":[{"type":"command","command":g+" relay","async":True,"timeout":10}]}] for e in ev}
st={"tui":"fullscreen","hooks":hooks,"statusLine":{"type":"command","command":g+" statusline","padding":0,"refreshInterval":30},
    "permissions":{"allow":["Bash(sleep:*)","Bash(echo:*)","Bash(date:*)"]}}
json.dump(st,open(L+"/tab-settings.json","w"),indent=2)
PY
cat > "$L/bin/claude" <<X
#!/bin/sh
exec $N/claude --setting-sources project --settings $L/tab-settings.json --model haiku "\$@"
X
chmod +x "$L/bin/claude"
# The tab's prompt, set at its first prompt, after ~/.bashrc (Giverny's bash init runs it).
cat > "$L/demo-rc" <<'X'
if [ -z "${__demo_rc-}" ]; then __demo_rc=1; PS1='\[\e[1;34m\]\W\[\e[0m\] \$ '; fi
X
"$R/feed.py" plan >/dev/null
cd "$A"
setsid env XDG_RUNTIME_DIR=/run/user/1000 systemd-run --user --scope --quiet --collect --unit="demo-530-$$" env -i \
  PATH="$L/bin:$N:/usr/local/bin:/usr/bin:/bin" HOME="$HOME" USER="$USER" SHELL=/bin/bash TERM=xterm-256color LANG=C.UTF-8 \
  PROMPT_COMMAND=". $L/demo-rc" \
  DISPLAY=:0 GALLIUM_DRIVER=d3d12 XDG_RUNTIME_DIR="$L/rt" DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  XDG_CONFIG_HOME="$L/xdg/config" XDG_DATA_HOME="$L/xdg/data" XDG_STATE_HOME="$L/xdg/state" XDG_CACHE_HOME="$L/xdg/cache" \
  GIVERNY_NO_ACCOUNT_SETUP=1 GIVERNY_DEMO_ACCOUNT=you GIVERNY_DEBUG_CMD="$L/cmd" CLAUDE_CODE_SUBAGENT_MODEL=haiku RUST_LOG=giverny=info \
  "$bin" >> "$L/giverny.log" 2>&1 < /dev/null &
for _ in $(seq 1 40); do p=$(pgrep -f -x "$bin" | head -1); [ -n "$p" ] && break; sleep 0.25; done
echo "$p" > "$R/run.pid"; echo "window pid $p"
