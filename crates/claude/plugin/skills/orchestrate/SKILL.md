---
name: orchestrate
description: Run a piece of work as a pass of subagents, one per task, several at once where their files do not overlap, and show it in Giverny's agents pane as Running, Next up with ETAs, and Done. Use when asked to orchestrate, fan work out to subagents, or work through a list of tasks in parallel.
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
- a **title**: one line saying what the task is.
- an **estimate** in minutes for how long its worker will take.

Group the tasks into **lanes** by the files they will touch. Tasks in different
lanes may run at the same time; tasks sharing files run one after another.

Record every task, in the order you mean to run it:

```bash
giverny-pass plan auth-fix --eta 25 --title "Fix the token refresh race"
giverny-pass plan docs-api --eta 15 --title "Document the new endpoints"
```

Tell the user the plan in a few lines (task, lane, estimate) before you start.

## 2. Start a task

For each task you start, stamp it and spawn its worker in the same breath:

```bash
giverny-pass start auth-fix
```

Then spawn **one subagent for this task** with the Agent tool:

- Its `description` must contain the task name as a whole word, e.g.
  `auth-fix: fix the token refresh race`. The pane joins the row to its worker
  by this name.
- Run workers in the background when several run at once, and never start two
  workers that touch the same files.
- Give the worker everything it needs in its prompt; it cannot see this
  conversation. Include these lines, with the task name filled in:

  > You are the worker for task `<task>`. Work only on this task and only in the
  > files it needs. If your estimate turns out wrong, say how many minutes are
  > left: `giverny-pass eta <task> <minutes> --note "<why>"`. Do not run
  > `giverny-pass start` or `giverny-pass land`; the dispatcher does. End with a
  > short report: what you did, how you checked it, and anything left undone.

## 3. While it runs

- When you learn a task will take longer or shorter, re-estimate it:
  `giverny-pass eta <task> <minutes left>`.
- If a worker has to wait on something outside the work (a person, a quota, a
  build slot), stop its clock with `giverny-pass pause <task> --note "<why>"`
  and restart it with `giverny-pass resume <task>`.
- A task you decide not to do comes out of the plan: `giverny-pass drop <task>`.
- `giverny-pass show` prints the pass as it stands.

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
