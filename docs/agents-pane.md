# The agents pane and its feed

The agents pane is a table under a tab's terminal listing that tab's Claude Code subagents: **Running**, **Planned** and **Done**. It works with no setup — Running and Done come from Claude Code's own data. An orchestrator that knows more (which task each worker holds, what is queued next, when each is due to land, who reviews it) can add that by writing a **feed**: one JSON file per Claude session, described here.

This document is the whole contract for writing a feed. The reader is `crates/claude/src/feed.rs`; anything this page promises, that module's tests pin.

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
| `eta_delta_s` | integer (may be negative) | no | Done rows only: how late (positive) or early (negative) the row landed against its estimate, in seconds. Send it only when you know better than Giverny's own arithmetic — e.g. a usage-limit wait that should not count. See **The Done row's ETA cell**. |
| `landing` | string | no | Where the row lands or landed — `Review — ita`, `Done`, `Blocked`. Drawn verbatim. |
| `tokens` | non-negative integer | no | Tokens spent. A live worker's own count takes precedence; this is the fallback (a Planned row has none; a Done worker Claude Code has forgotten still has this). |
| `group` | string | no | A lane or batch name. Informational; Giverny may draw rows of one group together. |
| `brief` | string (absolute path) | no | A file shown read-only when a **Planned** row is clicked. |
| `open` | string (shell command) | no | Run in a new tab when a **Running** or **Done** row is clicked, in place of Giverny's own transcript view (`giverny transcript --follow <agent jsonl>`) — e.g. `claude --resume <id>`. A command that resumes a conversation something is already running is not run: Giverny switches to the tab holding it, or says so. |
| `note` | string | no | Free text; shown for a Planned row with no `brief`, and as a tooltip otherwise. |

Timestamps and numbers are forgiving: a number sent as a numeric string (`"2400"`) is read, a fractional number is truncated, a negative `eta_s` or `tokens` reads as absent. `null` is the same as leaving the field out.

## Merge with the live rows

Claude Code reports each live subagent itself (id, name, status, start time, tokens, what it is doing now), and Giverny keeps the ones that finished until the tab's `/clear`. Those are the **live rows**. The feed's rows are joined to them on **`agent_id` = the live row's subagent id**:

1. **Every feed row is drawn**, in feed order, in the stage the feed gave it. The feed's stage wins over the live one: a worker that holds two tasks may have one Done and one still Running.
2. **A feed row whose `agent_id` matches a live row carries both.** The feed supplies `key`, `title`, `started`, `eta_s`, `landing`, `brief`, `open`; the live row supplies tokens and the current activity (read from the worker's transcript every second: the context it carries now, the count `orchestrate-status` writes), and its start time where the feed gives none. Where both have a value, the feed's `started` wins (it is per row, and moved on by the row's pauses) and the live row's tokens win.
3. **A feed row with no `agent_id`, or one Claude Code no longer lists, is joined by its `key` instead**: to the live row whose spawn `description` names the key as a whole word (`Work giverny#82 open direct` names `giverny#82`, never `giverny#820`). A description naming several keys is one worker holding each of them. A Running row takes a worker still running where there is one, any other row a finished one. So a writer that could not know the worker's id when it wrote the row — it wrote at spawn time and nothing has rewritten it since — still gets one row with the worker's tokens and activity. Only a row that matches nothing either way is drawn as the feed wrote it: its clock from `started`, its tokens from `tokens`. This is how Planned rows and long-finished Done rows appear.
4. **A live row no feed row carries is drawn on its own** — unless its description names a feed row's key, in which case it is that row's worker and is never drawn a second time. It is drawn as Running or Done by its own state, after that stage's feed rows. So a feed that describes only some workers leaves the rest visible.
5. **Sections are drawn Running, then Planned, then Done**; order within a section is feed order, then unmatched live rows in Claude Code's order.
6. **Dittos:** a row whose worker (`agent_id`) is the same as the row directly above it draws its per-worker cells — agent, ELAPSED, tokens, activity — as `"`. Give a worker's rows adjacent positions in `rows` to get that.

`aliases` are not part of the row merge; they only decide which file belongs to a tab. Giverny looks for `<current session id>.json` first, and if there is none, for the most recently modified `*.json` whose `session` or `aliases` contains the current id.

## Usage limits

While the tab's account is out of a usage limit — Giverny's own meters show a window at 100% with a reset still to come, or the tab stopped on a limit message — nothing runs, so the pane holds every **Running** row's clock: ELAPSED stops, the ETA stays where it was instead of counting down past zero, and NOW reads `5h limit → 13:00` (`7d limit → Mon 08:00` for a reset on another day, `limit` when nothing says when). Once the limit clears both clocks carry on from where they stopped: the span out is taken off the row's ELAPSED for as long as the pane remembers it (while Giverny runs). Planned ETAs are durations and do not move. A Done row is measured history and is left as it landed. A writer does not need to do anything for this. A writer that pauses rows itself for the wait (moving `started` on, with `paused_s`/`paused_since`) may: a row carrying either field is taken to have its stops in `started` already, and the pane's own hold is not applied to it again.

## The Done row's ETA cell

A Done row has nothing left to estimate, so its ETA cell says how the landing compared with the estimate: **`(+5m)`** landed five minutes late, **`(-1h20m)`** landed an hour twenty early, **`(±0m)`** on the minute. It is blank when there is no estimate.

- If the row has **`eta_delta_s`**, that is the value.
- Otherwise it is **`(ended − started) − eta_s`**, in seconds — `started` from the feed row, else the live worker's start time. If `ended` or `eta_s` or both start times are missing, the cell is blank.

Durations everywhere in the pane are written the way the ETA column writes them: whole minutes (rounded), units that are zero left out — `5m`, `1h3m`, `1h`, `1d3h12m` — with `~` in front of an estimate and none on a measured span.

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

- [ ] Path: `${GIVERNY_FEED_DIR:-<config>/giverny/feeds}/<parent session id>.json`, directory created if missing.
- [ ] Write to a non-`.json` temp name in the same directory, then rename.
- [ ] `"version": 1`, `"session"`, and every earlier id in `"aliases"`.
- [ ] Each row has `stage` and `key`; `agent_id` wherever a worker holds it.
- [ ] Timestamps RFC 3339 or epoch ms; `eta_s` in seconds from `started`.
- [ ] Done rows: `ended` (and `eta_delta_s` only when you know better).
- [ ] Rewrite on change, not on every tick.
