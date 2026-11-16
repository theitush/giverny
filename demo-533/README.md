# giverny#533: y0av/giverny #33-#41 on their own

The #530 demo re-filmed with no feed: nothing writes a feed or a manager mark, so the panel shows
only what the PRs do by themselves (live Running and Done rows, the worker page, the prompt box).
Same build as #530 (`task-530-demo` at `bbbc732`: `upstream/main`, the nine PR branches at their
heads, the debug-only `GIVERNY_DEBUG_CMD` driver and `GIVERNY_DEMO_ACCOUNT`).

The rig as it ran from `.claude/review/533/rig/` (paths are hard-coded there):

1. `run.sh` opens the window (refuses while another test window is open, or C: has under 5G),
   `setup.sh` starts each tab's work, then two `claude --bg` jobs (`nightly-triage`, `docs-sweep`).
2. `film.sh rec` (background), `film.sh start`, then the chunks in order:
   `rail1 rail2 away jobs panel page mgr land 'wake 770'`, `film.sh stop`; `close.sh` checks the hashes.
   The tab's claude gets `session-system.md` (which agents to start); nothing tells it how to pass
   a prompt-box line on.
3. `compose.ps1` turns the frames and `timeline.tsv` into a captioned sequence; Playwright's ffmpeg
   (`~/.cache/ms-playwright/ffmpeg-1013/ffmpeg-linux`, VP9/WebM, crf 18, read from `pipe:0`) encodes it.
4. `caps.py timeline.tsv` lists each caption's frames; `stills.ps1` cuts the page's stills.
