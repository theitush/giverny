# The agents pane and its feed

The agents pane is a table under a tab's terminal listing that tab's Claude Code subagents: **Running**, **Planned** and **Done**. It works with no setup — Running and Done come from Claude Code's own data. An orchestrator that knows more (which task each worker holds, what is queued next, when each is due to land, who reviews it) can add that by writing a **feed**: one JSON file per Claude session, described here.

Giverny ships that orchestrator itself. With `claude.agents_pane` on, every Claude session gets a `/giverny:orchestrate` skill (see **The orchestrator plugin**) and a `giverny-pass` command that writes the feed, so Running, Next up with ETAs, and Done need nothing but Giverny and Claude Code: no issue tracker, no `gh`, no scripts of your own. Any other writer that follows this page works the same way, next to it.

This document is the whole contract for writing a feed. The reader is `crates/claude/src/feed.rs`; anything this page promises, that module's tests pin.

## Writing it with `giverny pass`

`giverny pass` is the feed writer that comes with Giverny. The plugin puts it on the Bash tool's `PATH` as `giverny-pass`, so a session and its subagents can run it without Giverny being on `PATH`.

```
giverny pass plan  <task> --eta <dur> [--title T] [--note N] [--brief FILE] [--repo R]  # a Next up row
giverny pass start <task> [--eta <dur>] [--title T] [--agent <id>] [--repo R]           # Running from now; lands the worker's earlier task
giverny pass eta   <task> <dur left> [--note N] [--why wait|…]                         # re-estimate from now
giverny pass land  <task> [--outcome Done|Blocked|…] [--review TEXT] [--note N]  # Done now
giverny pass pause <task> [--note N]  /  giverny pass resume <task>          # stop / restart its clock
giverny pass drop  <task>  ·  giverny pass show  ·  giverny pass path  ·  giverny pass clear
giverny pass clear-done                                                        # clear the Done rows
```

- **A task is any short name** (`auth-fix`, `12`). It is the row's `key`. The worker's spawn `description` should name it as a whole word (`auth-fix: fix the refresh race`), which is how the row finds its worker (see **Merge**, rule 3).
- **Every time is measured.** `start` stamps `started`, `land` stamps `ended`, both from the machine's clock when the command runs. `<dur>` is minutes (`25`) or `25m`, `1h30m`, `1.5h`.
- **`start --agent` on a busy worker is a hand-off** (giverny#141). Its Running rows that started five minutes or more before land now (`Done`), so each task keeps its own measured span; rows started less than that before are a batch it was given at once and keep running.
- **`eta` takes the time left**, not a total. On a Running row it sets `eta_s` to the time worked so far plus that, so the pane's countdown shows it; the first estimate is kept as `eta_first_s`. On a Planned row it is the new estimate.
- **`eta --why wait`** (or `blocked`) marks the worker as waiting on something that is not the work, from now until its next `eta` without it, a `pause`, or `land`. The span goes to `wait_s` and is left out of the history's working time; the pane's clock does not move. A `pause` closes an open wait, so no span is counted twice.
- **Estimates learn from history** (giverny#143). Every row that lands (by `land`, or by any other command that leaves it Done) appends one line to `history.jsonl` beside the feeds, or to `$GIVERNY_PASS_HISTORY` (set it empty to turn learning off): its `key`, `title`, `repo`, `kind` (the title's leading upper-case type word, as `BUG` in `BUG: …`), `estimate_s` (the guess as given), `eta_s` and `eta_final_s`, `wall_s` (from the true start), `paused_s`, `wait_s`, `work_s` (wall less pauses and waits), and `outcome`. `plan` and `start` with `--eta N` then scale N by the median `work_s ÷ estimate_s` over the last 20 finished (`Done`/`Review`) tasks of the same repo and kind, else the same repo, else all, needing 5 at a level; the ratio is held in 0.25–4, and the figure rounds to whole minutes. `eta_s` gets the corrected figure, so the pane counts down from it; `eta_guess_s` keeps N and `eta_basis` says where the correction came from. Both are printed: `planned auth-fix: ~12m (you said 25m; ×0.48 from the last 9 BUG tasks in myapp)`, or `~25m as given (…too few to correct it)`. The repo is `--repo`, else the `<repo>` of a `<repo>#<n>` key, else the git checkout the command runs in (a worktree counts as its main checkout). Re-estimates by `eta` are never corrected.
- **The re-estimate nudge.** `giverny pass nudge` is the plugin's `PostToolUse` hook (see **The orchestrator plugin**). It reads the hook payload on stdin; for a call that is not a subagent's (no `agent_id` in the payload) it only delivers the session's messages and renews its leases (see **Resources**). For a worker's call it finds the dispatcher's feed from the payload's `session_id` and the Running row the worker holds (the row's `agent_id`, else a key its spawn description names). Once the row has run five minutes of working time with no `eta` from anyone, it stamps the row `reestimate_asked` and replies with an `additionalContext` asking the worker, once, to re-estimate with `giverny-pass eta`.
- **`land --review "<who> — <what> — <where>"`** writes the row's `review` line and a `landing` of `Review — <who>`. `--outcome` sets the landing word; `--note` is added after it.
- **`pause`/`resume`** write `paused_since`, then move `started` on by the span and add it to `paused_s`, with the true start kept in `spawned`, as **Schema** describes.
- **The session** is `--session <id>`, else `$CLAUDE_CODE_SESSION_ID`, which Claude Code sets in every Bash command it runs and a subagent inherits from its dispatcher. So a worker that re-estimates its own row writes into its dispatcher's file.
- **The file is the state.** Each command reads the feed, changes one row and writes it back atomically, under a lock (`<session>.json.lock`) so a dispatcher and its workers never lose each other's writes. A file whose bytes would not change is not rewritten.
- **`clear-done` clears the Done rows** (giverny#112): from this session's feed when `giverny pass` wrote it, and from the agents pane of the Giverny tab it runs in, whoever wrote the feed. The pane drops its own Done rows, keeps them from coming back from disk, and hides the feed's Done rows that landed before the clear. Running and Next up rows stay. In a Claude session the plugin's `/giverny:clear-done` runs it.
- **It never touches another writer's feed.** It marks its files `"writer": "giverny/pass"` and refuses any file whose `writer` names someone else.

## Resources

Before a worker starts, its orchestrator says what the worker needs, and the **machine ledger** answers. It is one file for every Claude session on the machine, so orchestrators in different tabs, repos and accounts share CPU, RAM, GPUs and exclusive slots without knowing about each other (giverny#159, #160).

```
giverny pass claim <task> [--cpu N] [--ram 3G] [--gpu N --vram 8G] [--slot NAME]… [--min-ram 2G] [--priority P]
giverny pass release <task>
giverny pass resources
giverny pass run <task> [--cpu N] [--ram 3G] [--slot NAME]… -- <cmd…>
giverny pass ask <task-or-session> "<msg>" [--task <yours>] [--priority P]
giverny pass reply <msg-id> "<text>"
```

- **`claim`** answers with one line and an exit code: `granted t1: 3 cpu, 3G, slot cargo:/x/target` (**0**); `granted smaller t1: 3 cpu, 2.5G (asked 3G, more is not free)` (**3**, only with `--min-ram`); `queued #2 behind giverny#12 (3G, slot cargo:/x/target; ~14m)` (**4**: who holds what it needs, and their time left from their own feed rows); `refused …` (**5**: larger than the limits, so never queued). 1 is an error, 2 a usage error. Without `--cpu` a claim asks for one core; without `--ram`, none. Sizes are `3G`, `1.5G`, `512M`; a bare number is GiB. A task that already holds a lease is answered with it (exit 0) — unless `--cpu`, `--ram` or `--vram` name a smaller figure than it holds and none a larger: then the lease **shrinks in place**, keeping its slots and its place (`shrunk giverny#12: 2 cpu, 1G, slot cargo:/x (was 3 cpu, 3G, slot cargo:/x)`, exit 0), which is how a holder makes room when asked (below). A held lease is never grown by `claim`; release it and claim again.
- **Queued means poll.** Nothing calls back: the orchestrator re-runs the same `claim` (each run keeps its place alive) until it is granted. The queue is first come, first served, a `--priority` (`asap` › `high` › `medium` › `low`) ahead of none. A request may go past one waiting ahead of it only with what is left after that one's request is set aside, so a small task can start beside a big one that waits, never take what it waits for.
- **The grant rule.** CPU: the request fits under the limit less every live lease, and under the machine's cores less the leases and the 1-minute load average the leases do not explain. RAM: under the limit less every live lease, and under `MemAvailable` less a headroom (5 % of RAM, at least 1 GiB) — that is how other programs count: a browser eating 6 G shrinks what is grantable. GPU: each GPU asked for has `--vram` free under its limit (leases share a GPU by VRAM). **Slots** are any name (`cargo:/path/to/target` for a shared cargo target dir) and exclusive: one lease holds a slot at a time.
- **Leases expire.** A lease (or a place in the queue) lives 20 minutes past its last heartbeat, and every `giverny pass` command from its session is a heartbeat, and so is every tool call of that session or its workers, through the plugin's hook (below; at most one ledger write per 30 s), so a working orchestrator keeps its leases without trying and a dead one's are dropped on the next read by anyone. `land` and `drop` release the task's lease; `release` does it by hand.
- **`resources`** prints the machine, the limits (`(auto)` where not set), other programs' load, what is free now, every lease with its session and time left, and the queue.
- **The feed row carries its lease** as the row's `lease` field (see **Row**), so the pane can show what each Running row holds and which Next up rows wait for resources.
- **The pane draws it** (giverny#164, #182). A row's lease is in its overlay header (`holds 3 cpu, 3G, slot cargo:/x/target`). A quiet column between TASK and ELAPSED shows what the row's `giverny pass run` commands **use**: on a Running row, while a command runs, its use now — `14% CPU 4.2G`, CPU as a share of the whole machine (every core busy is 100%) and the memory its scope's cgroup holds (`memory.current`), summed over the task's running commands, sampled every couple of seconds — and nothing while none runs. A Done row shows its memory peak alone (`2.1G`). A Next up row waiting in the ledger reads `queued for 3G behind giverny#12` in NOW. While anything on the machine is leased or queued, a dim line at the bottom right says how much of the limits is leased: `leased 4G/16.4G RAM · 4/12 cores · 1 queued` (each GPU too, when there is one). The overlay header adds the measured totals once a run has ended (giverny#173): `peak 2.1G of 3G, 45s CPU`. A run the memory cap killed adds `OOM` to the cell, drawn in the warning (amber) colour, and `OOM-killed x1` to the header. Only commands run through `giverny pass run` under a systemd scope are measured live; a plain run (no user systemd) shows its use only once it ends. The column is not there while no row has anything in it, nor the line while nothing is leased. **The ledger decides, not the copy:** the pane reads the ledger itself, off the UI thread, about every two seconds while a pane is up, and draws each row's entry from it — matched by the row's `key` under the feed's or the tab's session ids — so a lease that expired or was released, or a holder that left the queue, is gone from the pane though the row's copy still names it. A queued row is drawn behind the task its copy named while that one still holds or waits ahead, else behind whoever holds a slot it asks for, else behind the request queued before it.
- **When the ledger is not enough, ask** (giverny#162). The ledger is first come, first served; it cannot know that the holder of `cargo:/x/target` would gladly lend it for a high-Priority two-minute test. So when a queued wait is longer than the task itself — the soonest any holder in the way expects to land, against the asker's own time left (its estimate, for a Next up row), or a holder with no ETA at all — `claim`'s queued answer says so and suggests `giverny pass ask <holder's task> "<why>"`.
  - **`ask`** finds the session holding that task's lease in the ledger (else one queued under that name; or give a session id, or the start of one) and drops a message in that session's **inbox**, `<feed dir>/inbox/<session>.jsonl`, one JSON object per line. The message carries the asker's task (`--task`, else its one place in the queue, else its one lease), what it wants and its place in line, what it holds, its Priority (`--priority`, else its queued request's) and its time left by its own feed row. It prints the message's id (`m` and six characters).
  - **Delivery is the hook.** On each tool call of the orchestrator's own (not a worker's: the lease is the orchestrator's to give), the plugin's `PostToolUse` hook moves the session's unread messages to `<session>.seen.jsonl` (kept a day) and returns them as `additionalContext`: who asks, about which of the session's leases and what it holds, what the asker holds and wants, its Priority and time left, its words, and the three answers — `giverny-pass reply <id> "<answer>"`, `giverny-pass release <task>`, `giverny-pass claim <task> --cpu <fewer> --ram <less>` (shrink in place). A session busy only waiting on its workers sees it on its next own call.
  - **`reply <id> "<text>"`** answers a message (an ask, or a reply), found in this session's inbox files, else anyone's. The reply carries what the asked-about lease holds now, or that it was released, and reaches the asker the same way, with a reminder to re-run its claim.
  - **Messages expire with the asker's entry**: a message is delivered only while the asker still holds or waits for the task it asked for (a message from a session that held nothing lives 20 minutes). Undelivered, it is dropped at the next delivery. A writer other than `giverny pass` may append a line to an inbox under the sibling `<session>.json.lock` (taken with `O_CREAT|O_EXCL`, removed after), in the format `giverny pass ask` writes: `id`, `kind` (`ask`/`reply`), `at`, `from_session`, `from_task`, `to_session`, `about`, `text`, and optionally `in_reply_to`, `holds`, `wants`, `position`, `priority`, `eta_s`, `expires_with` (`{session, task}`).
- **`claim` says what tasks like it peaked at**, once the history has three with a measured peak (`run`, below): `granted giverny#12: 3 cpu, 3G (the last 4 BUG tasks in giverny peaked at 1.8G (median), 2.6G at most)` — the task's repo and title type word, else its repo, else every task.

**`run`: a worker's command under its lease** (giverny#161). A worker runs its heavy commands — builds, test suites, training — as `giverny pass run <task> -- <cmd…>`. Its own flags go before `--`; everything after is the command, untouched.

- **The cap.** On Linux with a user systemd the command runs in a scope of its own, `systemd-run --user --scope -p MemoryMax=<ram> -p MemorySwapMax=0 -p CPUQuota=<cpu×100>% -p OOMPolicy=continue`, named `giverny-run-<task>-<pid>.scope` (`systemctl --user status` it while it runs). `CARGO_BUILD_JOBS=<cpu>` is exported unless already set. A lease of no RAM sets no memory cap, of no cores no CPU quota. Without a user systemd (macOS, Windows, a container, `GIVERNY_RUN_NO_SYSTEMD=1`), or when `systemd-run` will not make the scope, the command runs plain and the lease is advisory; it says so.
- **No lease: claim one.** A task holding no lease gets one claimed by `run` — 3 cores and 3G unless `--cpu`/`--ram` say — and `run` gives it back when the command ends. While that claim is queued, `run` polls it every 15 s and the row is *waiting* (as with `eta --why wait`); refused (larger than the limits), the command does not run and `run` exits **5**.
- **Slots are held for the command's life.** For each slot of the lease, `run` takes an exclusive `flock` on `<ledger dir>/slots/<slot>.lock`, so two `run`s under one lease (a worker's parallel commands) take a slot in turn: the second says so and its row is *waiting* until it has it.
- **Heartbeat.** While the command runs, `run` beats its session's leases every 5 minutes, so a long build never outlives its lease's 20 minutes.
- **Live.** While the command runs, `run` keeps `<ledger dir>/runs/<pid>-<n>.live` (`task`, `session`, `pid`, `stats`, `started_ms`) and the shim writes its scope's cgroup (`cgroup <dir>`) into the stats file beside it as it starts; the pane reads the cgroup's `memory.current` and `cpu.stat` from there (giverny#182). The file goes when the command ends; one left by a killed `run` is skipped and swept by the next.
- **Measured.** When the command ends, `run` prints `exit 0; peak 1.2G of 3G, 45.1s CPU in 30.2s` and adds the run to the row's `usage` (see **Row**). Peak memory is the scope cgroup's `memory.peak` (it counts page cache too), read by a small `sh` shim inside the scope once the command has exited and while the scope still exists; without a scope it is the largest single process's peak RSS (`ru_maxrss`). CPU time is the user + system time of the command and every descendant it waited for. When the task lands, the row's peak, CPU time and OOM kills go into `history.jsonl` (`peak_mb`, `cpu_s`, `oom_kills`), which is where `claim`'s hint comes from.
- **OOM.** A command the memory cap killed (the scope's `memory.events` counts an `oom_kill`) is reported — `the 3G memory cap killed it (OOM). Ask for more: …` — so the worker can ask its orchestrator for more RAM and run it again.
- **Exit code** is the command's own, 128 + the signal when one killed it (137 for the OOM killer's SIGKILL). Ctrl-C reaches the command; `run` itself waits it out, so the run is still recorded.

**Limits** are what *all* orchestrators on the machine together may use. Set them in **Settings → Orchestrator → Limits**, where each field takes `auto`, a number or size (`6`, `16G`), or a share of this machine (`50%` of the cores or of the RAM, written to the file as the figure it comes to). They are `[orchestrator.limits]` in Giverny's `config.toml`, every key `"auto"` unless set:

```toml
[orchestrator.limits]
cpu_cores = "auto"   # cores − 2, at least 1; or a number
ram       = "auto"   # 70 % of RAM; or "16G"
gpus      = "auto"   # 90 % of each GPU's VRAM (nvidia-smi; none without it); or [{ index = 0, vram = "20G" }], or [] for none
```

**The ledger file** is `<feed dir>/resources/ledger.json` (`$GIVERNY_LEDGER` overrides). A writer other than `giverny pass` may read it, and may write it only the same way: take an exclusive `flock` on the sibling `ledger.lock` for the whole read-modify-write, drop expired entries, write to a temporary name in the same directory and `rename` it over. Version 1:

```json
{
  "version": 1,
  "leases": [
    { "id": "5c1e…:giverny#12", "session": "5c1e…", "task": "giverny#12", "repo": "giverny",
      "cpu": 3, "ram_mb": 3072, "gpus": [], "vram_mb": 0, "slots": ["cargo:/home/me/giverny/.claude/target-shared"],
      "granted_at": "2026-10-02T10:00:00Z", "heartbeat_at": "2026-10-02T10:12:30Z" }
  ],
  "queue": [
    { "id": "77d2…:inbar#5", "session": "77d2…", "task": "inbar#5", "cpu": 4, "ram_mb": 8192, "gpu": 0, "vram_mb": 0,
      "slots": [], "min_ram_mb": 4096, "priority": "high",
      "queued_at": "2026-10-02T10:05:00Z", "heartbeat_at": "2026-10-02T10:12:00Z" }
  ]
}
```

A lease's `id` is `<session>:<task>`, one per task per session; its `session` is the Claude session to ask for it. `ram_mb`/`vram_mb` are MiB; `vram_mb` is held on each GPU in `gpus`. Timestamps are RFC 3339 (epoch milliseconds are read too). An entry whose `heartbeat_at` is 20 minutes old is gone. A file of a newer `version` is refused, never overwritten.

## Where the file goes

```
$GIVERNY_FEED_DIR/<claude session id>.json
```

- **`GIVERNY_FEED_DIR`** is exported by Giverny into every tab, so a writer running inside a Giverny tab (or in a hook or subagent of a Claude session running there) just reads it. When it is unset or empty, the writer uses the default: `<config dir>/giverny/feeds`, where `<config dir>` is the platform's config directory — `~/.config` on Linux (`$XDG_CONFIG_HOME` if set), `~/Library/Application Support` on macOS, `%APPDATA%` on Windows. The writer creates the directory if it does not exist.
- **`<claude session id>`** is the session id of the *parent* Claude Code session whose subagents the rows describe — the `session_id` Claude Code puts on every hook payload and on the `subagentStatusLine` input. It is a file name, so use it verbatim.
- One file per session. Giverny never writes, renames or deletes anything in this directory; stale files are the writer's to clean up (or leave — an unmatched file costs nothing).

## Writing it: atomically

Write the whole file to a temporary name **in the same directory**, then `rename` it over `<session>.json`. The temporary name must **not** end in `.json` — use `<session>.json.tmp` or `.<session>.<pid>.tmp` — because Giverny scans `*.json` when it searches by alias.

```python
tmp = os.path.join(d, f"{sid}.json.tmp")
with open(tmp, "w") as f:
    json.dump(feed, f)
os.replace(tmp, os.path.join(d, f"{sid}.json"))
```

Giverny checks the file's mtime and size about once a second and re-reads it only when either changed. A file that fails to parse (a torn write from a writer that skipped the rename) is ignored and the last good read stays on screen, so the worst a non-atomic writer gets is a flicker of staleness — but do the rename.

Rewrite the file whenever anything in it changes. Do **not** rewrite it just to move the clocks: Giverny ticks ELAPSED itself every second from `started` (or from Claude Code's own start time for a live worker).

## Schema (version 1)

```json
{
  "version": 1,
  "session": "5c1e…",
  "aliases": ["0a9f…", "77d2…"],
  "rows": [
    {
      "key": "coo#158",
      "stage": "running",
      "title": "FEATURE: agents pane",
      "agent_id": "a93f0c1d2e",
      "started": "2026-09-23T10:00:00Z",
      "ended": null,
      "eta_s": 2400,
      "eta_delta_s": null,
      "landing": "Review — ita",
      "tokens": 123456,
      "group": "lane-1",
      "brief": "/home/me/.orchestrate/briefs/coo-158.md",
      "open": "claude --resume 5c1e…",
      "note": "waiting on the relay"
    }
  ],
  "footer": { "text": "session 3 · total: 1.2M" }
}
```

### Top level

| Field | Type | Required | Meaning |
|---|---|---|---|
| `version` | integer | no (absent = `1`) | The format version; see **Versioning**. |
| `session` | string | recommended | The Claude session id this feed is for — normally the file's own name without `.json`. |
| `aliases` | array of strings | no | Earlier session ids of the same conversation. Claude Code gives a conversation a new id on some resumes and restarts; a tab still holding an old id finds the feed through this list. List every id the conversation has had. |
| `rows` | array of row objects | no (absent = no rows) | The table, in the order the writer wants it drawn within each stage. |
| `footer` | object `{"text": string}`, or a bare string | no | One line drawn under the table, verbatim (a session count, a total). |

### Row

| Field | Type | Required | Meaning |
|---|---|---|---|
| `key` | string | yes, unless `agent_id` is given | What the row *is*: a task id (`coo#158`), a lane name, anything short. Drawn in the id column. When absent, `agent_id` stands in. |
| `stage` | string | **yes** | `running`, `planned` or `done` (case-insensitive; `queued`, `in progress`, `finished`, `completed`, `landed` are accepted as synonyms). Decides the section. A row with no stage, or one Giverny does not recognise, is dropped. |
| `title` | string | no | The row's text — the task title. Without it the pane shows the live worker's own description. |
| `agent_id` | string | no | The Claude Code subagent id holding this row — **the join key** with the live rows (see **Merge**). It is Claude Code's id for the worker: `tasks[].id` in the `subagentStatusLine` input, and the `<id>` in `<config>/projects/<cwd>/<session>/subagents/agent-<id>.jsonl`. Several rows may carry the same `agent_id` (one worker holding several tasks). |
| `started` | timestamp | no | When this row's work started. RFC 3339 string (`2026-09-23T10:00:00Z`, any offset) or integer epoch **milliseconds**. |
| `ended` | timestamp | no | When a Done row landed. Same formats. |
| `spawned` | timestamp | no | The row's true start, when `started` has been moved on by the spans it was paused (coo#170). Informational. |
| `paused_s` | non-negative integer | no | Seconds the row has spent paused, an open pause counted up to when the file was written. `started` is already moved on by them. Informational. |
| `paused_since` | timestamp | no | Running rows only: the row is paused now, since this instant. Its ELAPSED and ETA hold at the values they had when the file was written (its mtime) — `started` is taken to be true as of then — and NOW reads `paused since 12:58`. |
| `eta_s` | non-negative integer | no | The estimated total duration of the row, in seconds, measured from `started`. On Running and Planned rows it fills the ETA column (`~1h3m`); on Done rows it is what the landing is compared against. Giverny holds it through a usage limit itself (see **Usage limits**). |
| `eta_guess_s` | non-negative integer | no | `giverny pass`: the estimate as given to `plan`/`start --eta`, before the history corrected it into `eta_s`. Informational. |
| `eta_basis` | string | no | `giverny pass`: where the correction came from (`×0.48 from the last 9 BUG tasks in myapp`). Informational. |
| `repo` | string | no | `giverny pass`: the project the task belongs to, for the history. Informational. |
| `waiting_since`, `wait_s` | timestamp, non-negative integer | no | `giverny pass`: an open waiting span (`eta --why wait`) and the seconds of closed ones. Not drawn; the history leaves them out of working time. |
| `reestimate_asked` | timestamp | no | `giverny pass`: when the nudge asked the worker to re-estimate. Informational. |
| `eta_delta_s` | integer (may be negative) | no | Done rows only: how late (positive) or early (negative) the row landed against its estimate, in seconds. Send it only when you know better than Giverny's own arithmetic — e.g. a usage-limit wait that should not count. See **The Done row's ETA cell**. |
| `landing` | string | no | Where the row lands or landed — `Review — ita`, `Done`, `Blocked`. Drawn verbatim. |
| `tokens` | non-negative integer | no | Tokens spent. A live worker's own count takes precedence; this is the fallback (a Planned row has none; a Done worker Claude Code has forgotten still has this). A Done row of a worker that took tasks in turn shows its task's own count instead (**Merge**, rule 7). |
| `group` | string | no | A lane or batch name. Informational; Giverny may draw rows of one group together. |
| `brief` | string (absolute path) | no | A file shown read-only when a **Planned** row is clicked. |
| `open` | string (shell command) | no | Run in a new tab when a **Running** or **Done** row is clicked, in place of Giverny's own transcript view (`giverny transcript --follow <agent jsonl>`) — e.g. `claude --resume <id>`. A command that resumes a conversation something is already running is not run: Giverny switches to the tab holding it, or says so. |
| `note` | string | no | Free text; shown, with the row's key and title, for a Planned row with no `brief`, and as a tooltip otherwise. |
| `review` | string | no | Done rows: the one line a person has to read before the row counts (`<who> — <what> — <where>`). It is shown at the top of the row's overlay, verbatim, and nothing is fetched. See **The Review line**. |
| `usage` | object | no | `giverny pass run`: what the task's commands used, measured — `runs`, `peak_mb` (the highest peak of any run, MiB), `cpu_s` and `wall_s` (summed), `oom_kills`, and the last run's `last_cmd`, `last_exit`, `last_peak_mb`, `last_at`, `capped` (held by a systemd scope) with its `cap_cpu`/`cap_ram_mb`. Kept on the Done row; its peak goes into the history at landing. See **Resources**. |
| `lease` | object | no | `giverny pass claim`: what the machine ledger answered the row's task — `state` (`granted`, `smaller`, `queued`), `cpu`, `ram_mb`, `gpus` (indices), `vram_mb`, `slots`; granted rows add `id` and `granted_at` (and `smaller` ones `wanted_ram_mb`), queued rows `position` and `behind` (the holder it waits on). For a queued row the figures are the request. A copy as of the last `claim`, removed by `release`/`land`; the ledger is the truth. See **Resources**. |

Timestamps and numbers are forgiving: a number sent as a numeric string (`"2400"`) is read, a fractional number is truncated, a negative `eta_s` or `tokens` reads as absent. `null` is the same as leaving the field out.

## Merge with the live rows

Claude Code reports each live subagent itself (id, name, status, start time, tokens, what it is doing now), and Giverny keeps the ones that finished until the tab's `/clear`, a `giverny pass clear-done`, a fresh `claude` started in the tab (giverny#117), or a `/resume` into another conversation, which shows that conversation's own workers instead (giverny#112). A re-id of the same conversation — the agents view's ← →, the move into a background host, a compact — keeps them (giverny#105). Those are the **live rows**. The feed's rows are joined to them on **`agent_id` = the live row's subagent id**:

1. **Every feed row is drawn**, in feed order, in the stage the feed gave it. The feed's stage wins over the live one: a worker that holds two tasks may have one Done and one still Running.
2. **A feed row whose `agent_id` matches a live row carries both.** The feed supplies `key`, `title`, `started`, `eta_s`, `landing`, `brief`, `open`; the live row supplies the clock's start, tokens and the current activity — the first two exactly as Claude Code's own agents view has them, so the two agree (giverny#116). The clock counts from the worker's `startTime`, not from `started`, which a writer stamps a little before the spawn; `started` counts only for a row that began more than five minutes after its worker did (a later task given to the same worker), and a row's pauses move the worker's start on by as much as they moved `started` on (`started − spawned`, else `paused_s`). Tokens are Claude Code's `tokenCount` — the context of the worker's last turn plus all its output so far — read from each tick, with the transcript's context (re-read every second; the count `orchestrate-status` writes) as the floor: Claude Code's count is only ever below it when it is wrong, as after an API error, where it is the worker's output alone (giverny#92). Where both have a value, the live row's tokens win over the feed's `tokens`.
3. **A feed row with no `agent_id`, or one Claude Code no longer lists, is joined by its `key` instead**: to the live row whose spawn `description` names the key as a whole word (`Work giverny#82 open direct` names `giverny#82`, never `giverny#820`). A description naming several keys is one worker holding each of them. A Running row takes a worker still running where there is one, any other row a finished one. So a writer that could not know the worker's id when it wrote the row — it wrote at spawn time and nothing has rewritten it since — still gets one row with the worker's tokens and activity. Only a row that matches nothing either way is drawn as the feed wrote it: its clock from `started`, its tokens from `tokens`. This is how Planned rows and long-finished Done rows appear.
4. **A live row no feed row carries is drawn on its own** — unless its description names a feed row's key, in which case it is that row's worker and is never drawn a second time. It is drawn as Running or Done by its own state, after that stage's feed rows. So a feed that describes only some workers leaves the rest visible.
5. **Sections are drawn Running, then Planned, then Done**; order within a section is feed order, then unmatched live rows in Claude Code's order.
6. **Dittos:** a row whose worker (`agent_id`) is the same as the row directly above it, in the same batch (rule 7), draws its per-worker cells — agent, ELAPSED, tokens, activity — as `"`. Give a worker's rows adjacent positions in `rows` to get that.
7. **One worker, tasks in turn** (giverny#141). A worker given its next task by `SendMessage` holds a row per task, each with its own clock and its own tokens. A worker's Running and Done rows are grouped by when they began: rows that began within five minutes of each other are one **batch** — the tasks it was given at once (`Work #144 #145`), one clock and one count, dittoed — and each later batch is the task it was handed next.
   - **Each task closes the one before.** A row of an earlier batch ends where the next batch began, at the latest; a row still `running` in the feed is drawn Done there, its NOW cell `→ <next key>` unless it has a `landing`. So `start <next> --agent <worker>`, written when the message is sent, is the whole hand-off; `giverny pass start --agent` also lands the worker's earlier Running rows in the file.
   - **When a task began.** A row the spawn named (its worker's description names its key) began with the worker. A later one began at its `started` — or at the dispatcher's message naming its key first, read from the worker's transcript (a `user` line with `"origin":{"kind":"coordinator"}`), when that came earlier: a hand-off recorded late is still timed from when it was sent.
   - **Tokens per task.** A Running row keeps the worker's live count, as Claude Code's agents view shows it. A Done row of a worker with more than one batch shows only what its task added: over the worker's transcript turns from its batch's start (the first batch from the worker's first turn) to the next batch's, each reply once, its output plus the input read fresh (uncached input and cache creation; cache reads are carried context). The Done rows of a worker add up to what it added, nothing counted twice; the Running count is larger, since its context carries the earlier tasks. Where the transcript cannot be read the cell is blank, never the worker's whole count again.
   - **A hand-off nobody recorded.** A dispatcher's message whose first line says it is a new task (`New task for you: …`, `Next task: …`) and that names one gives the worker that task from then, with no feed write: a feed row with the key that is still waiting (Planned, or with no worker) becomes the worker's Running row; a key the feed lacks becomes a row of its own, titled with the message's first line, with no ETA (giverny#140); a worker the feed never described gets a row for the task it was spawned with too. When the worker has finished, its last task is Done with it.
   - **Queued on a worker.** A Planned row whose `agent_id` names a worker running another task says `after <that key>` in its NOW cell.

   The status line's `subagents` and `total` are per agent, each transcript read once; they are never the rows added up, so none of this changes them.

`aliases` are not part of the row merge; they only decide which file belongs to a tab. Giverny looks for `<current session id>.json` first, and if there is none, for the most recently modified `*.json` whose `session` or `aliases` contains the current id.

## Usage limits

While the tab's account is out of a usage limit — Giverny's own meters show a window at 100% with a reset still to come, or the tab stopped on a limit message — nothing runs, so the pane holds every **Running** row's clock: ELAPSED stops, the ETA stays where it was instead of counting down past zero, and NOW reads `5h limit → 13:00` (`7d limit → Mon 08:00` for a reset on another day, `limit` when nothing says when). Once the limit clears both clocks carry on from where they stopped: the span out is taken off the row's ELAPSED for as long as the pane remembers it (while Giverny runs). Planned ETAs are durations and do not move. A Done row is measured history and is left as it landed. A writer does not need to do anything for this. A writer that pauses rows itself for the wait (moving `started` on, with `paused_s`/`paused_since`) may: a row carrying either field is taken to have its stops in `started` already, and the pane's own hold is not applied to it again.

## Stopped workers

A worker that stops on an API error — no network (`EAI_AGAIN`), a usage limit, an expired login, a timeout — writes Claude Code's `<synthetic>` `isApiErrorMessage` line as its last turn. From that line on its **Running** row stands still: ELAPSED and the ETA hold, and NOW says why (`stopped: no network`, `stopped: usage limit`, `stopped: login expired`, `stopped: timed out`, `stopped: API error`). The same holds for a Running feed row whose worker Claude Code reports failed, killed or stopped (`stopped: failed`, …). A worker that finished cleanly is not held: its orchestrator's landing is still time on the task. When the worker is continued — Claude Code lists it running again, or it writes its next real turn — the clocks carry on from where they stood, with the stopped span taken off ELAPSED for good (the spans are saved with the row, so a restart keeps them). A writer does not need to do anything for this.

## The Done row's ETA cell

A Done row has nothing left to estimate, so its ETA cell says how the landing compared with the estimate: **`(+5m)`** landed five minutes late, **`(-1h20m)`** landed an hour twenty early, **`(±0m)`** on the minute. It is blank when there is no estimate.

- If the row has **`eta_delta_s`**, that is the value.
- Otherwise it is **`(ended − started) − eta_s`**, in seconds — the row's clock start as above (the worker's start, else `started`). If `ended` or `eta_s` or both start times are missing, the cell is blank.

Durations everywhere in the pane are written the way the ETA column writes them: whole minutes (rounded), units that are zero left out — `5m`, `1h3m`, `1h`, `1d3h12m` — with `~` in front of an estimate and none on a measured span.

## The Review line

When a **Done** row is clicked, the top of its overlay can carry one line saying what a person has to look at before the work counts.

- **The row's `review` field** is that line. It is shown as written. This is what `giverny pass land --review` writes, and it needs nothing else.
- **Without one, a `key` naming a GitHub issue** (`repo#12` or `owner/repo#12`) has the line read from the issue: one REST call through `gh` (`gh api repos/<owner>/<repo>/issues/<n>`), off the UI thread, taking the body's first `**Review:**` line above its first `---`. This only works where `gh` is installed and logged in. Where it is not, or the issue has no such line, the overlay opens without it.

## The orchestrator plugin

With `claude.agents_pane` on, Giverny carries a Claude Code plugin, `giverny`, whose one skill is `orchestrate`. Invoke it as `/giverny:orchestrate`. It is a generic dispatcher. It plans the tasks with estimates (`giverny-pass plan`), spawns one subagent per task with the task's name in the spawn description, stamps `start` and `land`, and has each worker re-estimate its own row. It does not rely on an issue tracker. It sits beside a project's own `/orchestrate` skill, if the project has one, because plugin skills are namespaced.

**How it is installed** (verified against Claude Code 2.1.283):

- The plugin is written as a local **directory marketplace** at `<config dir>/giverny/claude-plugin/` (next to `feeds/`). Its files are the marketplace manifest, the plugin manifest, `skills/orchestrate/SKILL.md`, and `bin/giverny-pass`, a small `sh` wrapper that runs `<this giverny binary> pass`. Only files whose bytes changed are rewritten, and it happens at each start with the pane on, so a moved or upgraded binary is picked up.
- Each account's `settings.json` gets two keys:
  ```json
  "extraKnownMarketplaces": { "giverny": { "source": { "source": "directory", "path": "<config dir>/giverny/claude-plugin" } } },
  "enabledPlugins": { "giverny@giverny": true }
  ```
  That is all Claude Code needs. There is no `claude plugin install` and no network. A directory marketplace is loaded straight from its directory at session start, not from a cache, so the next session runs what this binary wrote. The plugin's version is Giverny's own, so a Giverny upgrade updates the plugin. Sessions that were already running keep the skill they started with.
- `bin/` of an enabled plugin is on the `PATH` of the Bash tool, in the session and in its subagents, which is how `giverny-pass` is found.
- **Orchestrate by default** (`claude.orchestrate_by_default`, off by default, giverny#130). With it on as well as the pane, `hooks/hooks.json` also carries a `SessionStart` hook (matcher `startup|clear|compact`) whose command `cat`s `hooks/orchestrate-by-default.json`, a `hookSpecificOutput.additionalContext` telling the session to run any task it expects to take more than about a minute as a `/giverny:orchestrate` pass, one subagent per task, and to keep quick questions and reads inline. The text is `crates/claude/plugin/hooks/orchestrate-by-default.md`. Only sessions started, cleared or compacted after it is turned on get it. Off, that hook goes from `hooks.json` and its reply file is pruned with the rest of the stale files.
- **The re-estimate nudge** (giverny#143). `hooks/hooks.json` always carries a `PostToolUse` hook (matcher `*`) running `"${CLAUDE_PLUGIN_ROOT}/bin/giverny-pass" nudge 2>/dev/null || true`: about five minutes into a worker's task it asks that worker, once, for a fresh estimate (see **Writing it with `giverny pass`**). The same hook delivers an orchestrator's `ask`/`reply` messages and renews the calling session's ledger leases (giverny#162, see **Resources**). It exits 0 and prints nothing for every call that is not due anything, and reads no file for a call that is not a subagent's and has no message waiting: it parses only the payload's `session_id`, `agent_id` and `transcript_path`, `stat`s the inbox, the ledger and the session's heartbeat marker (`resources/beats/<session>`, whose mtime is the last beat; the ledger is locked, read and maybe written only when that is 30 s old). Measured in-process, that is about 8 µs a call with a 20 KB tool response, of which ~4 µs is the three `stat`s; the process start (~5–7 ms for the `giverny` binary) is the whole cost that matters.

**The house rules**, the same ones `subagentStatusLine` follows:

- **Opt-in.** Nothing is written until the pane is turned on.
- **Never over someone else's.** A marketplace called `giverny` that does not point at a `giverny/claude-plugin` directory is left alone. The plugin is then not installed on that account, and the settings log says so.
- **A user's `disable` stands.** If `enabledPlugins["giverny@giverny"]` is already `false` (`claude plugin disable`), it stays `false`.
- **Removed when the pane goes off.** Both keys go, and a map we emptied goes with them. Claude Code's own record of the marketplace (`plugins/known_marketplaces.json`) loses its `giverny` entry, as `claude plugin marketplace remove` would do. The `claude-plugin` directory is deleted. `uninstall_from` takes the keys too.
- **A no-op writes nothing.** If Giverny is deleted without turning the pane off, Claude Code finds the directory missing and skips the plugin silently.

## Where the live rows come from: the relay

With `claude.agents_pane` on, Giverny installs one key into each account's `settings.json`:

```json
"subagentStatusLine": { "type": "command", "command": "<giverny> relay --subagent-line" }
```

Claude Code runs that command at least every five seconds while a session has live workers. It passes the worker list on stdin: `session_id` and `tasks[]`. The relay forwards that list to the app, tagged with the tab it ran in (`GIVERNY_TAB_ID`, which reaches this command). It then prints one `{"id":"<task id>","content":""}` line per task. An empty decoration hides that row of Claude Code's own subagent panel. With every row hidden, the whole panel is gone, `● main` included. A brand-new worker may show for one tick (~300 ms) before it is hidden.

- **Outside a Giverny tab** (no `GIVERNY_TAB_ID`), the relay forwards nothing and prints nothing, so Claude Code draws its panel as usual.
- **With the setting off**, Giverny removes the key again, and the relay prints nothing even where the key is still there.
- **An account whose `subagentStatusLine` is someone else's** is left alone: the installer never replaces a command it did not write. On that account the pane gets no live rows.
- **Project settings override user settings.** A project that sets its own `subagentStatusLine` shadows Giverny's. To keep both, that command must pass through. Inside a Giverny tab (`GIVERNY_TAB_ID` set), pipe the stdin it received, byte for byte, into `giverny relay --subagent-line`, and print that command's stdout as its own. It must not add decorations of its own for ids the relay hid.

## Versioning

- `version` is bumped **only for an incompatible change** — a field whose meaning or type changes, or a new field a reader must understand to draw the table correctly.
- **Adding** a field is not a bump. Readers ignore fields they do not know, so a writer may send more than this page lists.
- A feed whose `version` is **greater** than the reader's is ignored entirely (the pane shows only the live rows) rather than half-understood. A missing `version` is read as `1`.
- The only things that make Giverny ignore a whole file are: it is not JSON, it is not a JSON object, or its `version` is too new. Anything smaller costs that field (a wrong type reads as absent) or that row (no usable `stage`, or neither `key` nor `agent_id`).

## Checklist for a writer

(`giverny pass` does all of this. The list is for writers of your own.)

- [ ] Path: `${GIVERNY_FEED_DIR:-<config>/giverny/feeds}/<parent session id>.json`, directory created if missing.
- [ ] Write to a non-`.json` temp name in the same directory, then rename.
- [ ] `"version": 1`, `"session"`, and every earlier id in `"aliases"`.
- [ ] Each row has `stage` and `key`; `agent_id` wherever a worker holds it.
- [ ] Timestamps RFC 3339 or epoch ms; `eta_s` in seconds from `started`.
- [ ] Done rows: `ended` (and `eta_delta_s` only when you know better).
- [ ] Rewrite on change, not on every tick.
