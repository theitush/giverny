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

Nothing else to set up. `/manager` shows or hides the band, `/manager clear-done` clears its Done rows, `/manager size <rows>` or `/manager size auto` sets its height, and `/manager view page` or `/manager view claude` sets what a worker's row opens.

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

The band is a copy of Giverny's management panel: the same columns in the same order and widths (stage, id and title, elapsed, ETA, NOW, tokens, use), no headings, the whole row in its stage's colour. Each column has a switch in `/config` (`column_stage`, `column_id`, `column_title`, `column_elapsed`, `column_eta`, `column_now`, `column_tokens`, `column_usage`, all on), which applies at once: a column off takes no width and the rest close up. As the terminal narrows, the title gives way and the rest runs off the right, as the panel's does.

**Height.** Drag the rule along the band's top, as the top edge of the panel, or type `/manager size <rows>`; `/manager size auto` gives it back to its rows. The height is kept across sessions. Never set, the band is as tall as its rows. The most it can be is what Claude Code gives the band, which caps the bottom of the screen at half the terminal, the prompt included: with workers about, 15 rows under the rule at 170x48 and 29 at 193x77, two more with none. Nothing lets a plugin past that.

**Scrolling.** A table taller than all that room scrolls under the wheel, with Claude Code's own `n more` line. Set shorter than its rows, the band is a window of its own with the panel's scroll bar beside it. Claude Code gives the band the wheel only when it is taller than the room, so a shorter band does not hear the wheel, which goes on scrolling the transcript. There the bar does it, or Ctrl+X Tab and then PageUp and PageDown, a window's worth each; Home and End do the same, as they cannot be told from those.

**Hover.** A row under the pointer takes a tint of its stage's colour and keeps its own ink, as the panel's does. The tint needs 24-bit colour: Claude Code runs at 256 colours unless the terminal says otherwise (Windows Terminal under WSL doesn't, so `export COLORTERM=truecolor` in the shell's startup file), and there the row takes a plain grey, one for every stage. A row the keyboard is on still inverts, for the ring. There is no press shade: a Button does not tell a plugin that it is held, and the row lights as on view at once when it opens. Half a second after the pointer comes onto a row, a tooltip with its whole title and each reason why it is where it is shows at the row's left edge: under the row, or over it where the band would clip it, and in a very short band as much as fits on the side with more room. It may cover rows while it shows and moves none. The pointer coming onto it puts it away, so the rows under it can still be reached. It does not follow the pointer's column: a hover carries no pointer, and only a `Client` row would, which has no keyboard walk.

**A press on a row** opens its page, and the row stays lit. To get back, press the `↺ back to manager` button on its own row above the rule, press 0 at the empty prompt, or press the row again. The rule stays whole, edge to edge. In a band with no row left to grow into, the back row takes the place of the window's last row.

**Pages.** In fullscreen a placed row draws the page and every other row of the transcript draws nothing while it is open, so it takes the transcript's area. Nothing else on the screen moves: no row and no prompt, before, during or after. A running worker's page follows its transcript each second, staying at the end unless you have scrolled up and following again at the end: the prompt, the words, each tool call with its result in a line, then what it is at and for how long. Its text is its transcript's, so it has no thinking and no tool's full output. A transcript past 4 MiB is read from its last 2 MiB, and a page draws its newest 80,000 characters.

A line typed at the real prompt goes where the band says: a worker's page sends it to the worker, running or with its turn ended (which wakes it), where it shows dim until the worker has read it; a Next up page rewrites it to the manager's `Start <key> now, adding this to its brief: …`; a page with nobody to send to refuses it. A line that did not go stays in the box. Slash commands run as anywhere. Esc at the prompt stops the manager's turn if one is running, and the band says so; a worker keeps running either way. To take a line a plugin has to drop it, and Claude Code says that: with no turn running it keeps a row in the manager's transcript, which no hook can remove, so the plugin words it `Sent to <worker>: <line>`, above the page; over a running turn it shows `Prompt dropped by a hook: Sent to …` in its notification line for a moment, out of any hook's reach.

What no hook can hide stands on or around the page: the logo above a short page, a slash command's echo, some notices, `Interrupted` after an Esc. While the manager writes, its text streams under the page and pushes it up, and goes when that message is done. A long brief opened at its top leaves the transcript off its end, with Claude Code's own line of the last prompt on the window's top row, until the next prompt or a scroll to the bottom. On the main screen, where what is drawn cannot be taken back, a page is a pane, with its box in the band.

**`worker_view`** (`/config`, or `/manager view page|claude`, at once and kept across sessions) can give a worker's row Claude Code's own view instead, `$.ui.view`. That view is the worker's own, thinking and full tool output included, and its prompt is that worker's. But Claude Code adds `○ main` and a blank under its prompt, which a plugin cannot stop: the band then keeps two blank rows under the table, and the prompt goes up two lines over a view. Esc at that prompt stops a running worker, and a plugin cannot change that. On the main screen a worker's row opens that view whatever the switch says.

**Use** is what a Running row's commands use now: its worker's Bash commands, with all they started, and its `giverny-manage run` scopes. CPU is a share of the whole machine, every core busy being 100%, and memory is proportional sets, shared pages split. A figure over 20% of the machine turns amber, over 50% poppy red. A Done row shows its peak memory. A command the session itself runs, or one two workers have in flight at once, belongs to no row. Linux only, as it reads `/proc`.

The plugin reads every process's `stat` every 4 seconds while a worker or a run is going, and nothing while none is, the band is hidden or the column is off. GPU memory is not shown.

## Known limits

- A new worker can show in Claude's list for a moment before it is hidden.
- `/manager` hides the band but not Claude's list, which stays hidden until the session restarts without the plugin.
- In a Giverny tab with the management panel turned off, the plugin still steps aside, so nothing replaces Claude's list.
- The `← 2 agents` hint is removed from the line under the prompt. The ← key was not changed.

## Developing

`dev/look.sh <fixture> [width]` shows the band off screen in a private tmux. Run `claude plugin validate .` and `claude plugin test .` before committing.
