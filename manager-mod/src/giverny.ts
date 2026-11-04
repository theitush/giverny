import type { EngineInterface, Settings } from 'claude-code'

// Living beside Giverny, and what a plugin can do about its permissions.
//
// Inside a Giverny tab the panel and its relay own the worker list: the plugin steps aside whole.
// Outside one, it takes the list over and lets the manager's own commands through.

/** Giverny exports this into each tab it starts. A claude nested under a tab inherits it too: see the README. */
export async function insideGivernyTab($: EngineInterface): Promise<boolean> {
  return Boolean(await $.env.get('GIVERNY_TAB_ID'))
}

/** Is Giverny's plugin on here, whose hook marks managers and tells them their chores? */
export async function givernyHookOn($: EngineInterface): Promise<boolean> {
  try {
    const enabled = (await $.settings.read({ key: 'enabledPlugins' })) as Record<string, unknown> | undefined
    return Boolean(enabled?.['giverny@giverny'])
  } catch {
    return false
  }
}

/**
 * Prints `{"id":…,"content":""}` per task, which hides Claude's own subagent list. Inline, as
 * `${CLAUDE_PLUGIN_ROOT}` does not reach this setting's command.
 */
export const HIDE_LIST = `grep -o '"id" *: *"[^"]*"' | sed 's/^"id" *: *\\(".*"\\)$/{"id":\\1,"content":""}/'`

/**
 * The merged settings as this session should run them. A `subagentStatusLine` in
 * `~/.claude/settings.json` beats one in a plugin's `settings.json`, and Giverny's relay sits
 * there, printing nothing outside a tab. So this answers the merge itself: the relay gives way
 * outside a tab, where it does nothing, and someone else's own command is left alone.
 */
export function withListHidden(settings: Settings, inTab: boolean): Settings {
  if (inTab) return settings
  const line = settings.subagentStatusLine as { command?: unknown } | undefined
  if (line && !/\brelay --subagent-line\b/.test(String(line.command))) return settings
  return { ...settings, subagentStatusLine: { type: 'command', command: HIDE_LIST } }
}

/**
 * `withListHidden` for a `settings.read` hook. Like every call on `$`, it is answered `{ value }`,
 * though the types name the bare settings; an answer of the bare settings is ignored without a word.
 */
export function answerMerge(answer: unknown, inTab: boolean): never {
  const { value } = answer as { value?: Settings }
  return (value ? { value: withListHidden(value, inTab) } : answer) as never
}

/** The dim line under the prompt without its `← 2 agents`: the band has them. */
export function hintWithoutAgents(hint: string): string {
  return hint.replace(/\s*(·\s*)?← \d+ agents?/, '')
}

/**
 * A shell command's simple commands, split on `;`, `&&`, `||`, `|`, `&` and newlines; or null where
 * it holds anything that could run or write something this cannot see: a substitution, a
 * subshell or group, a heredoc, a redirect other than to /dev/null or between stdout and stderr.
 */
export function splitCommand(command: string): string[] | null {
  const parts: string[] = []
  let part = ''
  let quote = ''
  const flush = () => {
    if (part.trim()) parts.push(part.trim())
    part = ''
  }
  for (let i = 0; i < command.length; i++) {
    const c = command[i] as string
    const after = command[i + 1]
    if (quote === "'") {
      part += c
      if (c === "'") quote = ''
    } else if (c === '\\') {
      part += c + (after ?? '')
      i++
    } else if (c === '`' || (c === '$' && after === '(')) {
      return null
    } else if (quote === '"') {
      part += c
      if (c === '"') quote = ''
    } else if (c === '"' || c === "'") {
      quote = c
      part += c
    } else if ('(){}<#'.includes(c)) {
      return null
    } else if (c === '>') {
      const redirect = /^>(&[12]|\s*\/dev\/null)(?=\s|$|[;&|])/.exec(command.slice(i))
      if (!redirect) return null
      part = part.replace(/(^|\s)[12]$/, '$1')
      i += redirect[0].length - 1
    } else if (c === ';' || c === '\n' || c === '|' || c === '&') {
      flush()
      if (after === c || (c === '|' && after === '&')) i++
    } else {
      part += c
    }
  }
  if (quote) return null
  flush()
  return parts
}

/** What `giverny-manage run <task> … -- <command>` runs, or undefined for any other command. */
export function runsCommand(part: string): string | undefined {
  if (!/^(giverny-manage|giverny\s+manage)\s+run\s/.test(part)) return undefined
  return /\s--\s+(.+)$/s.exec(part)?.[1]
}

const isManagerCommand = (part: string) => /^(giverny-manage|giverny-eta|giverny\s+manage)(\s|$)/.test(part)

/** Someone said this: a settings rule, a hook, a plugin or a setting; not just the mode's default. */
function saidBySomeone(v: { rule?: string; hook?: string; plugin?: string; setting?: string }): boolean {
  return Boolean(v.rule || v.hook || v.plugin || v.setting)
}

async function verdictOf($: EngineInterface, command: string) {
  return $.tool.check({ tool: 'Bash', input: { command } })
}

/**
 * The verdict for a Bash call, given the one beneath. A plugin's `settings.json` cannot grant
 * permissions and neither can an answer to `settings.read`, but `tool.check` can: without this a
 * worker's `giverny-manage eta` waits for a person. Only an `ask` the mode made by default turns
 * into an `allow`, and only where every part of the command is a manager command or already
 * allowed. A `run` lets its command through unless that is refused or asked by name.
 */
export async function letManagerThrough<V extends { decision: string; rule?: string; hook?: string; plugin?: string; setting?: string }>(
  $: EngineInterface,
  input: unknown,
  beneath: V,
): Promise<V | { decision: 'allow'; reason: string } | { decision: 'deny'; reason: string }> {
  if (beneath.decision !== 'ask' || saidBySomeone(beneath)) return beneath
  const parts = splitCommand(String((input as { command?: unknown }).command ?? ''))
  if (!parts || !parts.some(isManagerCommand)) return beneath
  for (const part of parts) {
    const inner = runsCommand(part)
    if (inner !== undefined) {
      const v = await verdictOf($, inner)
      if (v.decision === 'deny') return { decision: 'deny', reason: v.reason ?? `refused: ${inner}` }
      if (v.decision === 'ask' && saidBySomeone(v)) return beneath
    } else if (!isManagerCommand(part) && (await verdictOf($, part)).decision !== 'allow') {
      return beneath
    }
  }
  return { decision: 'allow', reason: "the manager plugin lets giverny-manage's own commands through" }
}
