---
name: manage
description: Run a piece of work as a manager session of subagents, one per task (a light worker reused for a next task in code it has already read), several at once where their files do not overlap, and show it in Giverny's management panel as Running, Next up with ETAs, and Done. Invoke it whenever the user says "manage" — "manage this", "manage that task", "and manage it plz", "/manage" — before doing any of the work yourself; "manage" means this skill, not a project's own orchestrate or dispatch skill. Also use it when asked to fan work out to subagents or work through a list of tasks in parallel.
---

# Manage a session

You are the dispatcher. You split the work into tasks, give each task its own
subagent, and keep the manager session's state in Giverny's management panel with
`giverny-manage`. The pane reads what `giverny-manage` writes: every row you plan
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

- a **task name**: short, unique in this manager session, letters, digits and `-` (`auth-fix`,
  `docs-api`, `12`). It is the row's name in the pane, and the link between the
  row and its worker.
- a **title**: one line saying what the task is. If the project types its
  tasks, start the title with the type word and a colon (`BUG: …`,
  `FEATURE: …`): your track record is told per type.
- an **estimate** in minutes for how long its worker will take, with your
  track record (below) taken into account. The figure you give is the figure
  the pane shows.

Group the tasks into **lanes** by the files they will touch. Tasks in different
lanes may run at the same time; tasks sharing files run one after another.

Write each task's **brief** to a file: the prompt you will give its worker
(see [Start a task](#2-start-a-task)), or at least the task's own text. Its
**Next up** row opens to it when clicked, so the user can read what each
worker will be told before it starts. Every `plan` gets one; a `plan` without
`--brief` says so.

Record every task, in the order you mean to run it:

```bash
giverny-manage plan auth-fix --eta 25 --title "Fix the token refresh race" --brief briefs/auth-fix.md
giverny-manage plan docs-api --eta 15 --title "Document the new endpoints" --brief briefs/docs-api.md
```

Keep the briefs somewhere that outlives the manager session (a scratch directory is
fine); a relative path is stored absolute. If the prompt changes before the
spawn, write the new one and pass it again on `start` (`--brief FILE`).

Each `plan` (and `start --eta`) prints the figure the pane will count down
from, which is the one you gave, and how your past guesses fared. Giverny
keeps a history of every task that landed: its estimate, its wall time and its
working time (wall time less pauses and waits). It takes the most recent 20
landed tasks like this one (same project and type, else same project, else
all; five at least) and tells you the median of working time ÷ estimate:

```
planned auth-fix: ~25m
  your last 9 BUG guesses in myapp took ×0.48 of what you said (median)
```

Nothing is corrected for you: take the factor into account in the figure you
give, and what you give is what the pane shows and what is scored. With too
little history the second line says so. Pass `--repo <name>` when the task
belongs to a project other than the directory you run in.

A worker's re-estimate is scored too, on its own track: its first `eta` on a
running task (the one Giverny asks for five minutes in, after it has read the
code) goes on the pane as given and is later checked against the working
time that was still to come. The ask, and the `eta` output, tell the worker
how its kind's re-estimates have fared, for it to weigh in its figure.

`giverny-manage accuracy` (optionally `--repo <name>`) shows every track,
older tasks against recent: the dispatcher's guess and the worker's
re-estimate. When the
user asks whether estimates are getting better, answer from it.

Tell the user the plan in a few lines (task, lane, estimate) before you start.

## 2. Start a task

First claim what its worker needs from the machine. Every Giverny session
shares one ledger of CPU, RAM, GPUs and exclusive slots, so a claim is how two
managers stay out of each other's way. You claim, because the lease is
yours; workers never do.

```bash
giverny-manage claim auth-fix --cpu 3 --ram 3G
```

Size it from the hint `claim` prints once there is history (`the last 4 BUG
tasks in myapp peaked at 1.8G (median), 2.6G at most`), else with the default
lease (Settings → Management panel; `giverny-manage resources` prints it). Add
`--slot <name>` for something only one task at a time may use (a shared build
folder), `--priority` when the task is urgent, `--gpu N --vram 8G` for a GPU. The answer is one line and an exit code:

- `granted` (0): go on.
- `granted smaller` (3, only with `--min-ram`): go on, with what it says.
- `queued` (4): something it needs is held. Start another planned task from a
  free lane instead, and re-run the same `claim` now and then until it is
  granted (nothing calls back). When it says the wait is longer than the task
  itself and suggests `ask`, send `giverny-manage ask <holder's task> "<why, the
  priority, how long you need it>"` and keep polling; the holder may release.
- `refused` (5): larger than this machine's limits (Settings → Management panel →
  Limits). Ask for less, or tell the user.

Then, once granted, stamp the task and spawn its worker in the same breath:

```bash
giverny-manage start auth-fix
```

Then spawn **one subagent for this task** with the Agent tool. One task, one
agent is the default. Reuse a worker instead only when **both** hold:

- it is **light**: under 100k tokens of context (the TOKENS column; `start`
  prints it for each idle worker), and
- the next task is **in the files it has already read, or follows on from what
  it just did**.

Otherwise spawn a fresh worker with a full brief. Every turn of a reused worker
re-reads its whole context, so a worker 300k tokens deep makes the next task
slow and expensive on every turn, and its old context crowds the new task. A
fresh worker that reads a few files costs far less than that. Hand a reused
worker over as in [Giving a worker its next task](#giving-a-worker-its-next-task),
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
  > if the figure stands: `giverny-manage eta <task> <minutes> --note "<why>"`.
  > Do the same whenever the estimate turns out wrong. When you are waiting on
  > something that is not the work (a build slot, a lock, a person), say so
  > with `--why wait`; your next `eta` without it ends the wait. Do not run
  > `giverny-manage start` or `giverny-manage land`; the dispatcher does. Run heavy
  > commands (builds, test suites, anything that eats CPU or memory) as
  > `giverny-manage run <task> -- <command>`: it keeps them inside what was
  > granted and measures them. Never set CPU or memory caps by hand. If `run`
  > says the memory cap killed your command (OOM), do not retry it: report that
  > to the dispatcher, who claims more. End with a short report: what you did,
  > how you checked it, and anything left undone.

  About five minutes into the task, Giverny's plugin puts a request for that
  re-estimate into the worker's context, once, unless it has re-estimated
  already. It asks again as each figure runs out: once when five minutes of
  it are left, and once if the work runs past it, unless the worker has just
  re-estimated. Each ask tells the worker how its kind's re-estimates have
  fared; none changes its figure. Its answer is the `eta` command.

## 3. While it runs

- When you learn a task will take longer or shorter, re-estimate it:
  `giverny-manage eta <task> <minutes left>`.
- If a worker has to wait on something outside the work (a person, a quota, a
  build slot), stop its clock with `giverny-manage pause <task> --note "<why>"`
  and restart it with `giverny-manage resume <task>`. Paused spans, and spans a
  worker marked with `eta --why wait`, are left out of the working time the
  history learns from; the pane still shows the wall time.
- A task you decide not to do comes out of the plan: `giverny-manage drop <task>`.
- A worker that reports an OOM needs a bigger lease: `giverny-manage release
  <task>`, then `claim` it again with more `--ram`, then send the worker on
  (`run` takes the new grant). A held lease never grows through `claim`. Once,
  not in a loop: if the bigger claim fails too, tell the user.
- **Another manager may ask for your resources.** When a line like
  `Giverny: another manager on this machine asks for resources (message
  mXXXXXX, …)` arrives in your context, weigh its priority and time left
  against your task's, then answer, always, even with no:
  - give way: `giverny-manage release <task>` frees everything, slots included
    (the worker's next `run` claims afresh and waits its turn), or
    `giverny-manage claim <task> --cpu <fewer> --ram <less>` shrinks in place and
    keeps the slots (a `run` already going keeps its cap; the next one uses the
    new grant), which does not help an asker waiting for your slot: release for that;
  - `giverny-manage reply <id> "<answer>"`, then carry on.

  The same hook tells you when a reply to your own ask arrives: re-run your
  `claim` at once.
- `giverny-manage show` prints the manager session as it stands.

### Giving a worker its next task

Only a light worker (under 100k tokens of context) whose next task is in code
it has read gets one; see [Start a task](#2-start-a-task) for why. To hand
such a worker its next task with `SendMessage` instead of spawning a new one,
record the hand-off in the same breath as the message, and run it **before**
you send the message:

```bash
giverny-manage start <next task> --agent <worker's agent id>
```

`start --agent` refuses a worker over the limit, naming its context: spawn a
fresh worker then. Pass `--heavy-ok` only when the task truly needs what that
worker has in its context and a brief cannot carry it.

That lands the worker's earlier task now, with its own measured time, and
starts the new one with its own clock; the pane shows each task as its own row,
and each finished row counts only the tokens spent on that task. A plain
`start <next task>` (with no `--agent`) works too when the message assigns the
task: it begins `New task for you: <next task>`, or names the task right after
a holding phrase (`Next you hold acme#614`, `Your next task is acme#614`, or `a
review round on #613` for `acme#613-r1`). The manager session and the pane
then link the row to the worker the message went to, and `start` points out the
idle worker to pass as `--agent`, with its context, and names any idle worker
over the limit as one to leave alone. A task the message only mentions
(`another worker now holds acme#615`) is never linked to it, and a row a
worker already holds — or that a worker was spawned for — is never moved by a
message; `start`/`eta --agent` is how to move one. Beginning the message with
`New task for you: <next task>` is what tells a hand-off to a worker still busy
on its last task apart from a mid-task note.

A Running row whose worker has finished says so on its row (`worker finished —
not landed`), and so does one with no worker at all (`no worker running`):
land it, or hand it to the worker that holds it.

## 4. Land a task

When a worker reports, check its work yourself before you call it done (read
the diff, run the tests). Then land it, with one of:

```bash
giverny-manage land auth-fix                                   # done
giverny-manage land auth-fix --outcome Blocked --note "needs the API key"
giverny-manage land ui-empty-state --review "<who> — <what to look at> — <where>"
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
them as the manager session's record. `giverny-manage clear` removes them when the user
asks.

## If `giverny-manage` is missing

The command comes with Giverny. If running it fails with "command not found",
carry on without it: the management works the same, the pane just shows the
workers without the plan and the estimates.

If it is there but has no `claim` (an older Giverny: `giverny-manage` prints its
usage with no `claim` in it), skip claiming, `run`, `ask` and `reply`, and carry
on as before; workers run their commands themselves.
