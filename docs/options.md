# Options

<!-- Generated from crates/core/src/settings.rs.
     Regenerate: cargo run -p giverny-core --example options -->

Everything in `~/.config/giverny/config.toml`, and everything in the settings screen (`Ctrl+,`) — they are the same list.

| Key | Default | What it does |
|---|---|---|
| `font.family` | `""` | Preferred monospace family; empty auto-detects. Takes effect on restart. |
| `font.size` | `13.0` | Point size of the terminal grid. |
| `theme.name` | `"monet-dark"` | Colour theme for the grid and the chrome around it. One of: monet-dark, monet-light, ink, tokyo-night, gruvbox, nord, catppuccin, catppuccin-mauve, rouen, phosphor, abyss, synthwave, workbench, riso. |
| `window.opacity` | `1.0` | How solid the window's background is; below 1.0 the desktop shows through. Takes effect on restart. |
| `titles.strip_host_prefix` | `true` | Drop the `user@host:` your shell puts in front of every title. |
| `titles.shorten_paths` | `false` | Abbreviate every directory but the last: ~/Dev/bobo becomes ~/D/bobo. |
| `behavior.scrollback_lines` | `10000` | Lines kept above the screen, per tab. |
| `behavior.history_per_tab` | `true` | Each tab's shell keeps its own history, back when the tab is restored. |
| `behavior.history_also_shared` | `false` | With history per terminal, also append each command to the usual history (bash). |
| `behavior.notifications` | `true` | Notify when Claude needs you in a background tab. |
| `behavior.prefer_x11` | `false` | Run under X11/XWayland instead of Wayland. Takes effect on restart. |
| `behavior.restore_claude` | `"auto"` | Re-run `claude --resume` in restored tabs. One of: auto, prompt, off. |
| `behavior.windows_shell` | `"auto"` | Which shell a new tab opens on Windows. One of: auto, wsl, powershell, cmd. |
| `behavior.restore_apps` | 26 programs | Full-screen programs a restored tab may start again by itself. |
| `behavior.extra_profile_dirs` | `[]` | Account directories kept somewhere Giverny would not find on its own. Takes effect on restart. |
| `claude.auto_mode` | `false` | Every new Claude session starts in auto mode instead of asking for each permission. |
| `claude.skip_resume_summary` | `false` | Skip Claude Code's offer to resume from a summary, and resume the full session. |
| `claude.agents_pane` | `true` | Show the tab's subagents — running, planned and done — in a table under the terminal. |
| `claude.resume_after_limit` | `false` | Pick a session back up when the usage window that stopped it reopens. |
| `usage.refresh_minutes` | `10` | Ask Claude Code to refresh an account once its numbers are this old. 0 never asks. |
| `agents_panel.orchestrate_skill` | `true` | Ship the /giverny:orchestrate skill with the plugin the agents pane installs. |
| `agents_panel.columns.stage` | `true` | STAGE: Running, Next up or Done. |
| `agents_panel.columns.id` | `true` | The task's id (a feed row's key, a subagent's name). |
| `agents_panel.columns.title` | `true` | The task's title, or a subagent's description: the column that takes the room left. |
| `agents_panel.columns.elapsed` | `true` | How long the row has worked: a stopwatch while it runs. |
| `agents_panel.columns.eta` | `true` | Time left on a Running row, the estimate of a Next up one, how late or early a Done one landed. |
| `agents_panel.columns.now` | `true` | What the row is doing now: its last tool call, a wait, a queue place. |
| `agents_panel.columns.tokens` | `true` | The tokens the row has used. |
| `agents_panel.columns.usage` | `true` | What the row's `giverny orchestrator-session run` commands use: live CPU and memory, a Done row's peak. |
| `agents_panel.lease.cpu_cores` | `3` | Cores a task's lease holds when nothing says otherwise. |
| `agents_panel.lease.ram` | `"3G"` | Memory a task's lease holds when nothing says otherwise. |
| `orchestrator.limits.cpu_cores` | `"auto"` | Cores all orchestrator sessions together may hand to workers. auto = all but 2. |
| `orchestrator.limits.ram` | `"auto"` | Memory all orchestrator sessions together may hand to workers. auto = 70 %. |
| `orchestrator.limits.gpus` | `"auto"` | GPUs and VRAM orchestrator sessions may use. auto = 90 % of each GPU's VRAM. |
| `update.check` | `true` | Ask GitHub whether a newer Giverny exists, hourly while it is open. |
