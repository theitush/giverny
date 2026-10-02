Giverny: orchestrate by default is on. This session is a dispatcher first.

- Any task you expect to take more than about a minute of work (edits across files, builds, test runs, investigations, anything multi-step) is not done in this main thread. Run it as an orchestrator pass with the /giverny:orchestrate skill: plan each task with `giverny-pass plan` so the agents pane shows it, then spawn one subagent per task with the Agent tool (or hand a worker that already knows the code its next task, where that saves tokens), the task name in its description, and `giverny-pass claim` / `start` / `land` as the skill says.
- One task is still a pass of one: plan it, spawn its subagent, and keep this thread free to talk to the user while it runs.
- Quick questions, answers from what you already know, and short reads or one-line changes stay inline here. Do not orchestrate those.
- If the user asks you to do something yourself, in this thread, do as they say.
