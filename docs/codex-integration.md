# Codex integration

Giverny runs the normal Codex CLI in terminal tabs. Type `codex` as usual;
there is no wrapper, shell alias, or Giverny-specific launch command.
Claude Code and Codex can run in different tabs in the same workspace.

## Setup

1. Install Codex and sign in with `codex login` using OpenAI's
   [CLI instructions](https://learn.chatgpt.com/docs/codex/cli).
2. Giverny detects the installation or existing Codex profile automatically.
   Open Settings (`Ctrl+,`) → **codex** to review the detected environment.
3. Review the displayed hook path, events and command, then choose
   **install / repair hooks** for the environment where Codex runs.
4. In Codex, open `/hooks` and review and trust the Giverny observer hooks.
   Restart Codex after setup. Giverny never bypasses hook trust or changes
   Codex's approval policy or sandbox.

`giverny codex-hooks install` installs the hooks from a shell;
`giverny codex-hooks remove` removes them. The CLI uses the Codex home
configured in Giverny, otherwise `CODEX_HOME`, otherwise `~/.codex`.
Detection is on by default. Set `codex.enabled = false` to opt out; an existing
explicit opt-out is preserved. Add `--enable` to the install command to turn
integration back on after opting out.

Installation merges into `hooks.json`, preserves other hooks and unknown
fields, and keeps a `.giverny-bak` backup of an existing file. Invalid JSON
is reported and left untouched. Repairing a changed hook command requires
reviewing the updated hook in Codex again. Existing inline hooks in
`config.toml` remain untouched; Codex may warn about using both representations.
Removal takes out only Giverny's observer entries.

This integration was developed against Codex CLI **0.163.0**. Install a version
supporting the documented [hooks](https://learn.chatgpt.com/docs/hooks) and
[app-server APIs](https://learn.chatgpt.com/docs/app-server). Missing APIs and
helper failures appear in the usage panel rather than preventing terminal use.

## State and notifications

Hooks identify each conversation by its session ID and inherited Giverny tab
ID, including when two tabs use the same directory. Session start is idle;
prompt and tool activity are busy; permission requests and supported question
tools need attention. A completed turn in a background tab gets a check mark.
Interrupts return to idle and session exit clears the live state.

The same rail indicators and attention shortcut work for Claude and Codex.
Desktop notifications follow `behavior.notifications`. Before hooks are
installed and trusted, native process detection can show that Codex is open,
but cannot identify its conversation or infer whether it is working.

Codex's shared-server mode remains enabled when the CLI selects it. Giverny
does not inject `--no-daemon`, alter the CLI's model or permissions, or take
over approvals. Subagent hooks do not mark the parent conversation complete.
Sessions outside Giverny have no tab identity and are not relayed.

## Usage

Usage is part of the integration: the rail shows the available allowance
windows, percentages used, reset countdowns, and age of the last successful
reading. Window labels come from the durations Codex reports; they are not
assumed to always be five hours and a week.

Giverny launches an owned `codex app-server --listen stdio://` helper and calls
`account/read` and `account/rateLimits/read`. Codex uses its existing sign-in
and handles authentication itself. Giverny never opens `auth.json`, reads the
OS keychain, implements OAuth refresh, or calls private usage endpoints.
The helper can contact OpenAI. No model turn is started to obtain usage.

The first reading is requested when Codex is detected. Discovery runs at startup
and periodically while Giverny is open. A machine without a detected Codex
installation or profile shows no Codex account group and starts no usage helper.
Automatic refresh defaults to ten minutes; the rail's refresh button requests
a reading immediately. Set `codex.usage_refresh_minutes = 0` for manual-only
refresh. Concurrent requests are coalesced and failed automatic requests wait
until the next interval. Requests have timeouts, and owned helpers are stopped
when cancelled or Giverny shuts down. User-owned daemons are never stopped.

Only ChatGPT-backed accounts with available allowance data have subscription
meters. Unsupported authentication, missing limits and login errors are shown
explicitly. A failed refresh retains the last reading as stale; unavailable or
expired readings are never presented as a fresh zero percent. Giverny stores
successful readings in its own `state/codex-usage.json` cache for restart and
diagnostics. This cache contains account display names, windows, timestamps and environment paths,
not credentials. Token totals and account-switching controls are outside this
initial integration.

## Restore

Giverny saves the Codex session ID, home, conversation directory and WSL
distribution with the tab. Existing Claude state files continue to load.
Restoration uses the normal `codex resume <id> --cd <directory>` command;
session IDs are validated and paths are quoted for the tab's shell.

Before restoring, a background helper reads session metadata with `thread/read`.
It never resumes a conversation merely to inspect it. `restore = "auto"`
reconnects the saved conversation using the normal CLI, including when the
shared daemon still holds it in memory. Loaded threads are not proof that
another terminal is open. `"prompt"` offers an explicit resume button and
`"off"` skips restoration. Missing sessions are reported and skipped.

Tabs still start lazily. Resume waits for shell startup, and typing cancels
pending automatic restoration.
On Windows, native PowerShell and WSL resume commands use the corresponding
shell syntax; cmd.exe filenames requiring unsafe expansion are refused.

## Portable configuration and Windows/WSL

Defaults work through PATH and `CODEX_HOME`; no particular package manager,
username, model or installation directory is assumed. `codex.executable` accepts
an executable name or explicit path. Set it if a desktop launch does not inherit
the PATH your shell uses. `codex.home` accepts an explicit home directory;
leave it empty to use the environment and standard default.

Windows discovers default Codex homes in installed WSL distributions as well
as the native home. Hook paths are accessed through the corresponding WSL
share, but helper processes run inside their distribution. A saved WSL session
returns to that distribution. A custom WSL home can be selected with its UNC
path in `codex.home`. Hook callbacks carry the distribution identity explicitly,
so identical Unix paths in different distributions remain distinct.

`giverny doctor` prints executable/version discovery, the configured home,
hook installation status, cached usage age and saved conversation references.
Hook trust must be inspected in Codex with `/hooks`; installation alone is not
proof that a hook is trusted or has delivered an event.

Linux is Giverny's primary tested platform. Platform code compiles in CI;
macOS and native Windows/WSL behavior still needs verification on real hardware.
