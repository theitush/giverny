# giverny#530: the demo of y0av/giverny #33-#41

Scratch branch, never for upstream: `upstream/main` with the nine PR branches merged, a debug-only
driver (`crates/app/src/demo_drive.rs`, `GIVERNY_DEBUG_CMD`) and the account shown as `GIVERNY_DEMO_ACCOUNT`.

The rig as it ran from `.claude/review/530/rig/` (paths are hard-coded there):

1. Build: `cargo-shared build -p giverny`, copied to `rig/giverny-debug`.
2. `run.sh` opens the window, `setup.sh` starts each tab's work, then two `claude --bg` jobs.
3. `film.sh rec` (background), `film.sh start`, then the chunks in order: `rail1 rail2 away jobs nextup page mgr land1 'start3 742' 'done1 780'`, `film.sh stop`.
4. `compose.ps1` turns the frames and `timeline.tsv` into a captioned sequence; Playwright's ffmpeg
   (`~/.cache/ms-playwright/ffmpeg-1013/ffmpeg-linux`, VP9/WebM only, read from `pipe:0`) encodes it.
