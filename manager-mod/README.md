# manager

A Claude Code plugin that shows a manager session's work as a band above the prompt, in plain Claude Code: what is **Running**, what is **Next up** with its ETA, and what is **Done**. It reads the rows from a Giverny feed file and the workers from Claude Code itself, and it replaces Claude's own list of subagents.

It needs Claude Code 2.1.318 or later. The plugin API it uses is early access, so a later release may break it. Linux and WSL only: the list is hidden by a `grep | sed` command, and some of what it reads comes from `/proc`.

## Install

From a copy of this folder:

```
claude plugin install manager --marketplace /path/to/manager-mod --scope user
```

Or type `/plugin install manager --marketplace /path/to/manager-mod` at the prompt of a session. The plugin is read from that folder itself, so do not move it. To try it for one session without installing, run `claude --plugin-dir /path/to/manager-mod`.

Installing from GitHub by `<owner>/<repo>` needs `.claude-plugin/marketplace.json` at the root of the repository. Here it sits in this folder, so that line works only once the plugin has a repository of its own.

Nothing else to set up. `/manager` shows or hides the band, and `/manager clear-done` clears its Done rows.

## What it needs

| You have | What you get |
|---|---|
| Claude Code alone | The band shows your live workers, Running and Done, with no ETAs or tasks. Claude's own list is hidden. |
| A writer of the feed | Rows, ETAs, Next up, the Review line and the rest. Giverny's `giverny-manage` is the writer today, and any program following `docs/management-panel.md` works too. |
| Giverny's plugin on (`giverny@giverny`) | Also `/giverny:manage`, ETA reminders, messages between managers, and chores told to the manager. |

`giverny-manage` is the `giverny` binary, which needs no running app but does come with the whole app. The plugin does not install it.

**A manager.** Only a manager session draws rows no worker stands behind: its Next up and its build rows. With Giverny's plugin on, a session is a manager once the manage skill has run in it, as in Giverny. Without that plugin nothing could record that, so a session becomes a manager when its feed holds a planned row. A session that only spawns workers never does.

## Permissions

A plugin's `settings.json` cannot grant permissions, so this plugin answers Claude Code's permission check (`tool.check`) for Bash. Without this, in the default permission mode, every worker's `giverny-manage eta` stops and waits for you.

It turns an **ask** into an **allow** only when both hold:

- The ask is the permission mode's default, not one made by a settings rule, a hook or another plugin.
- Every part of the command is one of `giverny-manage`'s own bookkeeping commands (`eta`, `plan`, `start`, `land`, `claim` and so on) or `giverny-eta`, or is already allowed. A command with a substitution, a subshell, a heredoc or a redirect to a file keeps its ask.

`giverny-manage run <task> -- <command>` gets no pass of its own, because it runs anything: its command is judged as if you had typed it, so a command you have not allowed still asks. With no allow rules, a worker stops for your answer on `cargo test`, as it would without this plugin. Giverny's own `Bash(giverny-manage:*)` rule is wider: with it in your settings, `run` runs anything without asking.

## Beside Giverny

| Where the session runs | Who draws | `subagentStatusLine` that runs |
|---|---|---|
| A Giverny tab (`GIVERNY_TAB_ID` set) | Giverny's panel. The plugin draws nothing, reads nothing and registers no command. | Giverny's relay |
| A plain terminal | This band | The plugin's, in place of the relay, which does nothing outside a tab |
| A `claude` started inside a tab, or under a `tmux` it launched | Neither: Claude's own list. It inherits `GIVERNY_TAB_ID`, so the plugin steps aside, and the relay ignores a session that is not the tab's own. | Giverny's relay |
| Any session with your own `subagentStatusLine` | This band, and Claude's list next to it | Yours. It is left alone. |

Only one such command runs per session, and one in `~/.claude/settings.json` beats one in a plugin's own settings. So the plugin answers Claude Code's read of the merged settings (`settings.read`), and no file is changed.

## Columns

The rows draw a mark, id, title, the ETA's gauge, elapsed, ETA, NOW, tokens and use. Each has a switch in `/config` (`column_stage`, `column_id`, `column_title`, `column_elapsed`, `column_eta`, `column_now`, `column_tokens`, `column_usage`, all on), which applies at once: a column off takes no width and the rest close up. The gauge goes with the ETA. As the terminal narrows, tokens go first, then the word `CPU`, then the use, then the rest as before.

**Use** is what a Running row's commands use now: its worker's Bash commands, with all they started, and its `giverny-manage run` scopes. CPU is a share of the whole machine, every core busy being 100%, and memory is proportional sets, shared pages split. A figure over 20% of the machine turns amber, over 50% red. A Done row shows its peak memory. A command the session itself runs, or one two workers have in flight at once, belongs to no row. Linux only, as it reads `/proc`.

The plugin reads every process's `stat` every 4 seconds while a worker or a run is going, and nothing while none is, the band is hidden or the column is off. GPU memory is not shown.

## Known limits

- A new worker can show in Claude's list for a moment before it is hidden.
- `/manager` hides the band but not Claude's list, which stays hidden until the session restarts without the plugin.
- In a Giverny tab with the management panel turned off, the plugin still steps aside, so nothing replaces Claude's list.
- The `← 2 agents` hint is removed from the line under the prompt. The ← key was not changed.

## Developing

`dev/look.sh <fixture> [width]` shows the band off screen in a private tmux. Run `claude plugin validate .` and `claude plugin test .` before committing.
