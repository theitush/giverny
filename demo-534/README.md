# giverny#534: the demo of what agents-pane adds on top of y0av/giverny #33-#41

Scratch branch, never for upstream: `origin/agents-pane` with the #530 driver (`crates/app/src/demo_drive.rs`, here on
`GIVERNY_DEMO_CMD`, since agents-pane's own `debug_cmd` owns `GIVERNY_DEBUG_CMD`) and a debug-only `GIVERNY_DEMO_PLUGIN`:
a side instance (`GIVERNY_NO_ACCOUNT_SETUP=1`) still writes its own plugin into its own isolated config, which the test
claudes load with `--plugin-dir`. The rig ran from `.claude/review/534/rig/` (paths are hard-coded there).

1. Build: `giverny-manage run demo-fork -- cargo-shared --then 'cp … rig/giverny-debug' build -p giverny`.
2. `run.sh keep` opens the window (own XDG, runtime, feed, ledger and history dirs; `[manager.limits]` as the real
   config's; claudes on project settings, haiku, no MCP). The scratch project is `~/giverny/demo-534/acme`, outside any
   `.claude` path (Claude Code asks for every edit under one). `acme/` here holds its files, the plan and the briefs.
3. Rehearsal rounds first (their landed tasks are the history the gut lines come from), then `film.sh rec` in the
   background and `take4.sh`: the whole take, event-driven (waits on `giverny-manage show`, approves this demo's own
   prompts with `approve.sh`, which reads the screen through `dump`).
4. `edl.txt` → `cut.py` → timeline; `compose.ps1` (captions drawn in PowerShell) → Playwright's ffmpeg, VP9 crf 38, 10 fps.

Pitfalls: pointer glides queue one event a frame, so a long glide lags the script (use ~250 ms); a worker's view open in
the manager tab takes typed lines (`take4.sh`'s `main_view` returns first); `date -d today` breaks after midnight.
