# 186-mechanics: can a mod run the orchestrator?

**Short answer: mostly yes.** A mod running in Claude Code 2.1.288 can spawn parallel workers of its own agent type, message a running worker, run the whole plan → spawn → wait → land → next loop with the model idle, wake the session when a decision is needed, read the 5-hour and weekly limits, and run `systemd-run --scope` with streamed output. The weak spots are all about **state that lasts longer than one session**:
- a hot reload wipes the loop's in-memory state, though the workers keep running;
- `/clear` changes the session id;
- `$.store` loses updates when two sessions read-modify-write the same key;
- `$.agent.list` forgets finished workers within about 2.5 minutes;
- `$.fs` cannot append, rename or lock.

None of these blocks the port. Each one means writing the loop as a state machine that saves itself, not as a script.

Setup: the mod is `pass-spike/` (`hooks/register.ts`, ~240 lines). It ran in two interactive sessions in tmux (`spikeA`, `spikeB`), both `claude --plugin-dir pass-spike --model haiku`, with cwd set to a scratch folder. Every observation the mod made is one JSON line in `logs/A.jsonl` or `logs/B.jsonl`. Each line carries `t` (ms epoch) and `load` (a random id minted on every module load, so you can see which environment logged it). Screen captures are in `captures/`. `claude plugin validate pass-spike` passes (only the "no author" warning).

---

## 1. Parallel workers: **yes**

- **They run concurrently.** `/spike-par` called `$.agent.spawn` three times inside one `Promise.all`, each with `subagentType: 'pass-spike:worker'`. All three spawns resolved in 300 ms, each with its `{model, agentId}` (`logs/A.jsonl`, `par.spawned`). Their `sleep 30` Bash calls started at …043.07, …043.50 and …044.16 and ended at …076.15, …076.44 and …077.11, about 33 s each. Run one after another they would have taken 99 s. Session B and the `/clear` run show the same.
- **The main session stays usable.** While the three slept, I typed "Reply with the single word PONG." and got `PONG` within 5 s (main `turn.complete` at …059.25, while all three workers were mid-`sleep`). See `captures/A-01-par.txt`. Workers spawned by the mod do **not** start a main-model turn when they finish: no turn followed their `turn.complete`s. The model costs nothing while it waits.
- **The mod can register its own agent type.** `$.agent.register({name:'worker', prompt, tools:['Bash'], model:'haiku', omitClaudeMd:true})` returned `{agent:'pass-spike:worker'}` (log `agent.register`). Spawns resolved to `claude-haiku-4-5-20251001`, and `agent.list` shows `type: "pass-spike:worker"`. `on('agent.offer', {agent}, () => ({isOffered:false}))` hides the type from the model while the mod can still spawn it.
- **What `$.agent.list()` reports:** `{id, description, type, status: running|completed|…, spawnedBy: 'pass-spike'}`. It has **no tokens, no times and no answer.**
  - Gotcha: **finished entries are dropped.** In B, with no reload, the list held the three as `completed` 1 s after they finished (…486.9). 164 s later it was `[]` (…649.7, same `load` id `ehl5mi`). A reload empties it as well (A at …263 and …444). Any Done column has to be the mod's own list.
- **What `turn.complete` reports**, once per worker, keyed by `agentId`: `reason`, `durationMs`, `answer` and `usage` = `{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, model}`. Example: `DONE1`, 35.9 s, 216 output tokens. That is everything tokens.rs and worker_log.rs dig out of transcripts.
- **Live activity: yes, one `tool.call` event per worker tool call**, carrying `agentId`, `tool` and the input. A hook wrapped around `next(e)` gets both start and end, with duration and `isError` (`tool.call.start` / `tool.call.end` lines). `turn.step` also streams every model request, if a pane needs finer detail.
  - Gotcha: `tool.call` also fires for the engine's own **AgentSummary forks**. These are agent ids that `agent.list` never names (e.g. `a322c74fbe05b5352`), each making a denied Bash call about 30 s into a worker's run. The debug log shows `[AgentSummary] Timer fired for agent …`. Filter on the agentIds you spawned.

## 2. Inbox: **yes, and the worker acts on it mid-run**

`/spike-inbox 8` spawned a worker told to "sleep 15, sleep 15, reply DONE + any extra instruction". 8 s later, `$.clock.after` called `$.session.append({agentId, message:{type:'user', content:[{type:'text', text:'NEW INSTRUCTION … run echo INBOX-OK …'}]}})`:
- The append resolved with the row stored (`isMeta: true`, a uuid) while the first sleep was still running (log `inbox.appended` at …111.3).
- At the worker's next step (…122.1), it started the second `sleep 15` **and** `echo INBOX-OK`.
- Its answer was `DONE. Run echo INBOX-OK, end reply with PINEAPPLE.`

The message reaches the worker at the top of its next loop, not in the middle of a tool call. This replaces `pass_inbox`'s file plus the PostToolUse nudge for every worker in the same session. A message from another session still needs a shared channel (see 4).

## 3. Pass loop without a babysitting model: **yes, with care around reloads**

`/spike-loop` sets up three tasks and two slots, all in module memory and mirrored to `$.store`.
- **Where the loop lives.** `fillSlots` spawns. A `turn.complete` hook matches the `agentId`, "lands" the task (a `$.process.run(['git','rev-parse'])` standing in for a merge) and fills the freed slot. Clean run (log `loop.*`, `captures/A-02-inbox-loop.txt`): t1 and t2 started together; t1 landed at …170.28 and t3 was spawned 0.1 s later; t2 landed at …174.5; t3 landed at …183.4; then `loop.done`. The main model took no part.
- **Waking the session works.** t3 answered `NEED-DECISION-t3`, and the hook called `$.prompt.submit(…)`. The idle session started a turn shown as "Prompt from the pass-spike plugin / The pass-spike plugin sent a message: …", and the model answered `GO` (main `turn.complete` at …185.0).
- **Hook budget.** Each hook gets 10 s of **its own code time** (`HookBudget.ms`). Time spent in `$` calls and in `next()` does not count, but a `$.clock.sleep` does. A `/spike-proc 20` command that streamed a child for 20 s inside one `command.run` hook finished fine (22 pieces, …417.5 → …437.5). `next.signal` aborts when the dispatch is abandoned. Long-lived work belongs in timers (`$.clock.every/after`) started from `session.start`, plus event hooks. That is how the loop above is built: no hook waits for a worker.
- **Hot reload (I saved `register.ts` mid-loop)** — the main gotcha:
  - The workers kept running.
  - `session.start` fires again on a reload, so a new environment (`k41ddk`) registered.
  - That new environment received both workers' `turn.complete`.
  - Its module-level `loop` was `null`, so **nothing landed and t3 was never spawned. The loop stalled silently.** `$.store` still held the loop with t1 and t2 `running` (log `agent.list` at …263, `storedLoop`). The fix is cheap: keep loop state in `$.state` / `$.store` and rehydrate it in `session.start`.
  - The old environment did **not** stop at once. Its timers kept firing (`po25u3` heartbeats until …228.5) until its last in-flight hook (a worker's `tool.call`) finished at …230.2. For about 20 s both environments' timers ran.
- **`/clear` mid-run (three workers sleeping):**
  - `session.end` fired with `reason:"clear"` and listed the three as `running`.
  - The workers carried on, and their `turn.complete`s reached the mod.
  - Both timers kept ticking.
  - No `session.start` fired.
  - **The session id changed** (`0203024f…` before, `144e404f…` after; see the `store.race` rows). Any store key built from the session id is orphaned.
- **Timers drift.** The 10 s heartbeat slipped to 12.8 s while a main turn ran (…047.4 → …060.2). Fine for a poll, not a clock.

## 4. Cross-session view: **partly**

- **One file for all sessions.** The store is a single JSON file per plugin source: `~/.claude/plugins/store/pass-spike_inline-02a1dd4e6b5b.json`. Sessions A and B both wrote there, and A read B's row written about 225 ms earlier (`store.race`: A's `rows` holds both A's and B's session ids). Polling works. There is **no change event**, so a view polls on a `$.clock.every`.
- **Race on one key: updates lost.** A and B each ran 200 get+1/set increments of the same key at the same time. The final value was **200, not 400** (B last saw 160, A 200; `logs/*.jsonl` `store.race`). `$.store` has no compare-and-swap. `$.state.set(…, {ifVersion})` has one, but it is per-session.
- **Race on disjoint keys: nothing lost.** A and B each wrote 150 keys of their own: all 300 survived (`store.keys`; the store file has 150 `k:A…` and 150 `k:B…`). The store merges per key.
- **So:** cross-session rows work if each session writes **only keys it owns** (`row:<agentId>`), and a reader lists `keys()` and polls. Shared counters and ledgers need a lock the store cannot give.
- **Would our own JSON file via `$.fs` be better? No.** `$.fs` has `read`, `write` (whole file), `list`, `exists`, `stat` and nothing else: no append, no rename, no lock. Concurrent writers would clobber each other. The working options are `$.process.run(['tee','-a',f])` for append-only logs (as this mod logs) or `flock` through `$.process.run` for a ledger.
- **Coverage.** A mod only sees the sessions that load it. To match "Giverny sees every session on the machine", it would have to load everywhere, through `CLAUDE_CODE_PLUGIN_DIRS` in `~/.claude/settings.json` `env` or an installed plugin. Even then, sessions without it (other config dirs, other machines) are invisible.

## 5. Rate limits / usage: **yes, for the session's own account**

- **Where Giverny gets them today.** `crates/core/src/limits.rs` is **not** the plan limits: it is the CPU, RAM and GPU limits for orchestrators (giverny#159). The plan figures come from `crates/claude/src/usage.rs`, which reads `~/.claude.json → cachedUsageUtilization`, and from the statusline's `rate_limits`, which `hooks.rs` forwards to `claude_watch.rs`.
- **What the mod sees.**
  - `$.session.usage()` returns `rateLimits: [{kind:'five_hour', percentUsed:3, resetsAt:'2026-10-03T13:40:00.000Z'}, {kind:'seven_day', percentUsed:20, resetsAt:'2026-10-08T10:00:00.000Z'}]`, plus `context` and `cost` (log `usage`).
  - The `session.measure` event pushes the same figures with no polling: at session start, after each main turn, and whenever a window moves a whole point. The log caught `seven_day` 20 → 21 and `five_hour` 3 → 4 during the inbox run.
- **Gaps.**
  - It covers only the account this session runs under. usage.rs reads every profile.
  - `kind` is limited to `five_hour`, `seven_day` and a gateway's `spend_limit`. Per-model weekly buckets would need `$.fs.read('~/.claude.json')`, as today; they were all null on this account just now.

## 6. Resources: **yes**

`/spike-proc 20` ran `$.process.spawn({argv:['systemd-run','--user','--scope','--quiet','-p','MemoryMax=200M','-p','MemorySwapMax=0','--','sh','-c', …]})`. Output streamed piece by piece, one tick per second, and the child's `/proc/self/cgroup` was `…/app.slice/run-r95f99….scope`, so it ran inside its own capped scope (log `proc`). `$.process.run` takes the same argv for one-shot commands; its timeout is 10 min at most, so long builds go through `spawn`. Leaving the loop, `next.signal` or a module unload kills the child. A build started from a hook therefore dies on a hot reload unless it is detached (`systemd-run --unit … --no-block`, then watched).

The neater port: a mod can hook a **worker's** Bash calls (`tool.call` with that worker's `agentId`) and rewrite `e.command` to wrap it in the lease's `systemd-run`. The cap would then apply without the worker knowing about `giverny-pass run`. I did not try this; see the last list.

## Other gotchas seen

- **Workers inherit Ita's settings hooks.** `omitClaudeMd` drops CLAUDE.md, but the giverny plugin's context still reached the haiku workers, and two of them ran `giverny-pass eta agent-… --agent …` unprompted (log `tool.call.start` …181.2, …227.3). A mod-run pass should give its agent type explicit `hooks`, or retire the plugin's worker instructions.
- **`--allowedTools Bash` was ignored** ("Ignoring dangerous permission Bash(*) from cliArg (bypasses classifier)" in the debug log). Workers still ran Bash here, but an unattended pass has to settle the permission mode explicitly (`AgentSpec.permissionMode`).
- **The API is early access** ("may change between releases without notice"). A port is pinned to Claude Code's release train.

---

## Port sketch

Sizes are rough TypeScript line counts for the mod. Rust line counts are from integ.

| module (lines) | verdict | why / what remains | TS size |
|---|---|---|---|
| `subagents.rs` (2440) | **unnecessary** | `$.agent.list`, `turn.complete` and `tool.call` give live rows directly. One thing to keep: a short list of our own, so finished rows stay until /clear (the engine's list drops them within about 2.5 min). | ~80 |
| `feed.rs` (1821) | **unnecessary** in-session | The mod is both writer and reader of its own state (`$.state`). A cross-session view becomes reading `row:*` keys from `$.store` by polling. | ~100 |
| `transcript.rs` (619) | **mostly unnecessary** | `$.session.messages({agentId})` returns `{role, text, toolUses}` rows (from the types; not exercised here). Only the rendering is left. | ~100 |
| `pass_nudge.rs` (615) | **unnecessary** | The `tool.call` hook knows the agentId and the elapsed time, and `$.session.append` asks for the re-estimate. | ~40 |
| `pass_inbox.rs` (747) | **unnecessary** in-session, **port** across sessions | `$.session.append` to a running worker works (2). Asking another session's orchestrator still needs a shared channel: an append-only file through `tee -a` plus polling, or `$.prompt.submit` on the receiving side. | ~120 |
| `pass.rs` (2306) | **port, much smaller** | The feed writer and CLI become in-mod state plus registered tools (`mcp__pass-spike__eta`, …) that workers call instead of `giverny-pass eta`. Lock and atomic-write code goes, since there is one writer per session. | ~350 |
| `pass_history.rs` (799) | **port** | Pure logic: median correction over history.jsonl. Appending goes through `tee -a` (`$.fs` has no append), with the 4 MiB read limit in mind. | ~150 |
| `tokens.rs` / `worker_log.rs` (688 / 390) | **unnecessary** | `turn.complete.usage` per agent, per turn. | ~30 |
| `usage.rs` + statusline plumbing (551 + part of hooks.rs / claude_watch.rs) | **unnecessary** for own account | `$.session.usage()` and `session.measure`. Multi-profile meters cannot be done this way, except by reading `.claude.json` per profile through `$.fs`. | ~40 |
| `pass_run.rs` (1172) + `run_live.rs` (319) | **port, partly** | `systemd-run` scope and streaming both work (6). Best form: wrap worker Bash calls through `tool.call` rewriting. Live CPU/RAM figures by reading cgroup files through `$.fs`. The stats/OOM bookkeeping comes across. | ~250 |
| `resources.rs` (1400) | **cannot be mod-native safely** | A ledger shared by every orchestrator needs atomic read-modify-write across processes. `$.store` loses updates (4) and `$.fs` has no lock. Keep it as a small binary (or `flock` through `$.process.run`) that the mod calls. | keep Rust (~1400) or ~300 TS + flock |
| `limits.rs` (645) | **port or keep** | Config plus machine detection (`/proc/meminfo`, `nvidia-smi` through `$.process.run`). Trivial either way; it belongs with whatever holds the ledger. | ~80 |
| `agents_pane.rs` (3296, app) | **port** as a `Pane` | Running / Next up / Done rows drawn from `$.state`, buttons for open-transcript, toast on wake. | ~400 |

Roughly 1,700–2,000 lines of TypeScript plus the ledger kept in Rust would replace about 15k lines of the pass/feed/subagent stack, and Giverny would no longer need to be running for any of it. What a mod **cannot** give:
- a view of sessions that do not load it;
- a lock across processes;
- loop state that survives a reload or `/clear` without the mod rehydrating it itself;
- a stable API (it is early access).

## Not done here
- I did not try spawning from a mod-registered **tool** that the model calls; every spawn came from a slash command or a hook.
- I did not try rewriting a worker's Bash through `tool.call` to apply the cap (the "neater port" in 6).
- I did not code or test rehydrating loop state after a reload or /clear; it is design only.
- I did not call `$.session.messages({agentId})`, so the transcript row of the port sketch rests on the types alone.
- I did not try a long-lived headless SDK host (`CLAUDE_CODE_PLUGIN_DIR_WATCH`). All runs were interactive tmux sessions.
