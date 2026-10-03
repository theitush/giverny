---
name: orchestrate
description: Run a piece of work as a pass of subagents, one per task (a worker reused for a next task where that saves tokens), several at once where their files do not overlap, and show it in Giverny's agents pane as Running, Next up with ETAs, and Done. Use when asked to orchestrate, fan work out to subagents, or work through a list of tasks in parallel.
---

# Orchestrate a pass

You are the dispatcher. You split the work into tasks, give each task its own
subagent, and keep the pass's state in Giverny's agents pane with
`giverny-pass`. The pane reads what `giverny-pass` writes: every row you plan
shows under **Next up** with its estimate, a started row counts its time under
**Running**, and a landed row shows under **Done** with how it came out against
its estimate. Your own clock is not needed: every command stamps the time it
runs.

If the project has its own rules for picking or finishing work (a task list, a
CONTRIBUTING file, instructions from the user), those decide *what* to do;
this skill only says how to run it and keep the pane true.

## 1. Plan

Break the work into tasks a single subagent can finish on its own. For each,
choose:

- a **task name**: short, unique in this pass, letters, digits and `-` (`auth-fix`,
  `docs-api`, `12`). It is the row's name in the pane, and the link between the
  row and its worker.
- a **title**: one line saying what the task is. If the project types its
  tasks, start the title with the type word and a colon (`BUG: …`,
  `FEATURE: …`): estimates are corrected per type.
- an **estimate** in minutes for how long its worker will take: your honest
  guess, not one you have already adjusted.

Group the tasks into **lanes** by the files they will touch. Tasks in different
lanes may run at the same time; tasks sharing files run one after another.

Write each task's **brief** to a file: the prompt you will give its worker
(see [Start a task](#2-start-a-task)), or at least the task's own text. Its
**Next up** row opens to it when clicked, so the user can read what each
worker will be told before it starts. Every `plan` gets one; a `plan` without
`--brief` says so.

Record every task, in the order you mean to run it:

```bash
giverny-pass plan auth-fix --eta 25 --title "Fix the token refresh race" --brief briefs/auth-fix.md
giverny-pass plan docs-api --eta 15 --title "Document the new endpoints" --brief briefs/docs-api.md
```

Keep the briefs somewhere that outlives the pass (a scratch directory is
fine); a relative path is stored absolute. If the prompt changes before the
spawn, write the new one and pass it again on `start` (`--brief FILE`).

Each `plan` (and `start --eta`) prints what the pane will count down from.
Giverny keeps a history of every task that landed: its estimate, its wall time
and its working time (wall time less pauses and waits). Once there are enough
landed tasks like this one (same project and type, else same project, else
all), your guess is scaled by how long such tasks really took against their
estimates:

```
planned auth-fix: ~12m (you said 25m; ×0.48 from the last 9 BUG tasks in myapp)
```

The pane shows the corrected figure and the row keeps your guess, so the
guess's own bias stays measurable. Use the corrected figure when you tell the
user the plan. With too little history it says `as given`. Pass
`--repo <name>` when the task belongs to a project other than the directory
you run in.

Under it, `plan` says how your guesses for such tasks have fared:

```
  your last 9 BUG guesses in myapp took ×0.48 of what was said (median): they run long: estimate lower
```

Read it before the next guess, and let it move your raw figure too: the
correction only fixes the bias it has already seen.

A worker's re-estimate is scored too, on its own track: its first `eta` on a
running task (the one Giverny asks for five minutes in, after it has read the
code) goes on the pane as given, not corrected, and is later checked against
the working time that was still to come. The ask, and the `eta` output, show
the worker how its kind's re-estimates have fared, so the figure itself
improves.

`giverny-pass accuracy` (optionally `--repo <name>`) shows every track,
older tasks against recent: the dispatcher's guess, the pane's corrected
start figure, and the worker's re-estimate. When the
user asks whether estimates are getting better, answer from it.

Tell the user the plan in a few lines (task, lane, estimate) before you start.

## 2. Start a task

First claim what its worker needs from the machine. Every Giverny session
shares one ledger of CPU, RAM, GPUs and exclusive slots, so a claim is how two
orchestrators stay out of each other's way. You claim, because the lease is
yours; workers never do.

```bash
giverny-pass claim auth-fix --cpu 3 --ram 3G
```

Size it from the hint `claim` prints once there is history (`the last 4 BUG
tasks in myapp peaked at 1.8G (median), 2.6G at most`), else with a sane
default. Add `--slot <name>` for something only one task at a time may use (a
shared build folder), `--priority` when the task is urgent, `--gpu N --vram 8G`
for a GPU. The answer is one line and an exit code:

- `granted` (0): go on.
- `granted smaller` (3, only with `--min-ram`): go on, with what it says.
- `queued` (4): something it needs is held. Start another planned task from a
  free lane instead, and re-run the same `claim` now and then until it is
  granted (nothing calls back). When it says the wait is longer than the task
  itself and suggests `ask`, send `giverny-pass ask <holder's task> "<why, the
  priority, how long you need it>"` and keep polling; the holder may release.
- `refused` (5): larger than this machine's limits (Settings → Orchestrator →
  Limits). Ask for less, or tell the user.

Then, once granted, stamp the task and spawn its worker in the same breath:

```bash
giverny-pass start auth-fix
```

Then spawn **one subagent for this task** with the Agent tool. One task, one
agent is the default. Reuse a worker instead when that saves tokens or simply
makes sense: the next task is in code it has already read, or follows on from
what it just did. Hand it over as in [Giving a worker its next task](#giving-a-worker-its-next-task),
never as a second task folded into the first.

When you spawn one:

- Its `description` must contain the task name as a whole word, e.g.
  `auth-fix: fix the token refresh race`. The pane joins the row to its worker
  by this name.
- Run workers in the background when several run at once, and never start two
  workers that touch the same files.
- Give the worker everything it needs in its prompt; it cannot see this
  conversation. Include these lines, with the task name filled in:

  > You are the worker for task `<task>`. Work only on this task and only in the
  > files it needs. Once you have read the code (Giverny will ask you about
  > five minutes in), re-estimate once with how many minutes are left, even
  > if the figure stands: `giverny-pass eta <task> <minutes> --note "<why>"`.
  > Do the same whenever the estimate turns out wrong. When you are waiting on
  > something that is not the work (a build slot, a lock, a person), say so
  > with `--why wait`; your next `eta` without it ends the wait. Do not run
  > `giverny-pass start` or `giverny-pass land`; the dispatcher does. Run heavy
  > commands (builds, test suites, anything that eats CPU or memory) as
  > `giverny-pass run <task> -- <command>`: it keeps them inside what was
  > granted and measures them. Never set CPU or memory caps by hand. If `run`
  > says the memory cap killed your command (OOM), do not retry it: report that
  > to the dispatcher, who claims more. End with a short report: what you did,
  > how you checked it, and anything left undone.

  About five minutes into the task, Giverny's plugin puts a request for that
  re-estimate into the worker's context, once, unless it has re-estimated
  already. Its answer is the `eta` command.

### A worker spawned outside a pass

The same goes for *any* worker you spawn for a task, even one-off, mid-conversation,
with no plan behind it: run `giverny-pass start <task> --eta <minutes> --title "<title>"`
before the spawn (and `--agent <id>` once you know it). That row has the worker's
whole time and an estimate corrected from the history. If you forgot, Giverny asks
the worker itself on its first tool call for a first estimate, which it reports
with `giverny-pass eta <task> <minutes> --agent <id>`. Any subagent gets that ask,
pass or no pass, so the pane shows no worker without an ETA for long; but the time
before the ask is not counted, and the figure is not corrected from the history.

## 3. While it runs

- When you learn a task will take longer or shorter, re-estimate it:
  `giverny-pass eta <task> <minutes left>`.
- If a worker has to wait on something outside the work (a person, a quota, a
  build slot), stop its clock with `giverny-pass pause <task> --note "<why>"`
  and restart it with `giverny-pass resume <task>`. Paused spans, and spans a
  worker marked with `eta --why wait`, are left out of the working time the
  history learns from; the pane still shows the wall time.
- A task you decide not to do comes out of the plan: `giverny-pass drop <task>`.
- A worker that reports an OOM needs a bigger lease: `giverny-pass release
  <task>`, then `claim` it again with more `--ram`, then send the worker on
  (`run` takes the new grant). A held lease never grows through `claim`. Once,
  not in a loop: if the bigger claim fails too, tell the user.
- **Another orchestrator may ask for your resources.** When a line like
  `Giverny: another orchestrator on this machine asks for resources (message
  mXXXXXX, …)` arrives in your context, weigh its priority and time left
  against your task's, then answer, always, even with no:
  - give way: `giverny-pass release <task>` frees everything, slots included
    (the worker's next `run` claims afresh and waits its turn), or
    `giverny-pass claim <task> --cpu <fewer> --ram <less>` shrinks in place and
    keeps the slots (a `run` already going keeps its cap; the next one uses the
    new grant), which does not help an asker waiting for your slot: release for that;
  - `giverny-pass reply <id> "<answer>"`, then carry on.

  The same hook tells you when a reply to your own ask arrives: re-run your
  `claim` at once.
- `giverny-pass show` prints the pass as it stands.

### Giving a worker its next task

To hand a worker that knows the code its next task with `SendMessage` instead
of spawning a new one, record the hand-off in the same breath as the message:

```bash
giverny-pass start <next task> --agent <worker's agent id>
```

That lands the worker's earlier task now, with its own measured time, and
starts the new one with its own clock; the pane shows each task as its own row,
and each finished row counts only the tokens spent on that task. Begin the
message with `New task for you: <next task>`, so the pane can tell the hand-off
apart from a mid-task note even where the `start` was missed.

## 4. Land a task

When a worker reports, check its work yourself before you call it done (read
the diff, run the tests). Then land it, with one of:

```bash
giverny-pass land auth-fix                                   # done
giverny-pass land auth-fix --outcome Blocked --note "needs the API key"
giverny-pass land ui-empty-state --review "<who> — <what to look at> — <where>"
```

Use `--review` when the work is finished but a person has to look at it before
it counts: anything visual, a judgement call between two good options, an
irreversible change. The text is one line naming who, what exactly, and where
to see it; the pane shows it at the top of the row.

Landing also gives the task's lease back; there is nothing else to release.
Then start the next planned task whose lane is free, until none are left.

## 5. Finish

When every task has landed, tell the user what landed, what is blocked or
waiting for review, and what was left undone. Leave the rows: the pane keeps
them as the pass's record. `giverny-pass clear` removes them when the user
asks.

## If `giverny-pass` is missing

The command comes with Giverny. If running it fails with "command not found",
carry on without it: the orchestration works the same, the pane just shows the
workers without the plan and the estimates.

If it is there but has no `claim` (an older Giverny: `giverny-pass` prints its
usage with no `claim` in it), skip claiming, `run`, `ask` and `reply`, and carry
on as before; workers run their commands themselves.
