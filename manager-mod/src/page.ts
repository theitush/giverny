import type { PageEntry } from '../types'

import { describeToolUse } from './transcript'

// What a worker's page lists, plain data with no `$`: from `$.session.messages({ agentId })`,
// or from its transcript file where the session cannot read it (a worker of an earlier run).
// As crates/claude/src/transcript.rs draws it: the prompt, the words, one line per tool call.

type Use = { tool: string; input: Record<string, unknown>; isError?: boolean; text?: string }
type Message = { role: 'user' | 'assistant'; text: string; toolUses: readonly Use[] }

/** The room a page's texts have: a tree draws its first 100,000 characters and cuts the rest. */
export const ROOM = 80_000

// How Claude Code wraps a line that reaches a worker in the middle of its turn.
const WRAPPERS: [string, RegExp][] = [
  ['The coordinator sent a message while you were working:\n', /\n\nAddress this before completing your current task\.$/],
  ['The user sent a new message while you were working:\n', /\n\nThis is how Claude Code surfaces messages the user sends mid-turn[^]*$/],
]

const RESULT_MAX = 110

/**
 * A tool's result in one line: its first line with words on it, and how many more it has. As Claude
 * Code's own row shows one folded; the whole text stays in the worker's transcript.
 */
export function shortResult(text: string | undefined): string | undefined {
  if (text === undefined) return undefined
  const lines = text.split('\n').filter(l => l.trim() !== '')
  if (lines.length === 0) return '(no output)'
  const first = lines[0]!.trim()
  const head = [...first].length > RESULT_MAX ? `${[...first].slice(0, RESULT_MAX - 1).join('')}…` : first
  return lines.length > 1 ? `${head}  (+${lines.length - 1} lines)` : head
}

/** The line inside Claude Code's wrapper; text in no wrapper it knows is whole. */
export function unwrapped(text: string): string {
  for (const [head, tail] of WRAPPERS) {
    if (text.startsWith(head)) return text.slice(head.length).replace(tail, '')
  }
  return text
}

// A user message the worker reads and no person wrote: a reminder, a shell's notification.
const isMachine = (text: string) => /^\s*<[a-z-]+>/.test(text)

export function entriesOf(messages: readonly Message[]): PageEntry[] {
  const out: PageEntry[] = []
  for (const m of messages) {
    const text = m.text.trim()
    if (m.role === 'user') {
      if (text.startsWith('[Request interrupted')) out.push({ kind: 'tool', text: 'Interrupted' })
      else if (text !== '' && !isMachine(text)) out.push({ kind: 'prompt', text: unwrapped(text) })
      continue
    }
    if (text !== '') out.push({ kind: 'text', text })
    for (const use of m.toolUses) {
      // The worker's report is what it said last, whichever tool carried it.
      const said = use.tool === 'SubagentHandback' ? use.input.message : undefined
      if (typeof said === 'string') {
        out.push({ kind: 'text', text: said })
        continue
      }
      const result = shortResult(use.text)
      out.push({ kind: 'tool', text: describeToolUse({ name: use.tool, input: use.input }), ...(use.isError ? { isError: true as const } : {}), ...(result === undefined ? {} : { result }) })
    }
  }
  return out
}

type FileBlock = { type?: unknown; text?: unknown; name?: unknown; id?: unknown; input?: unknown; tool_use_id?: unknown; content?: unknown; is_error?: unknown }

/** A tool result's text in a transcript file, a string or text blocks. */
function resultText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return (content as { text?: unknown }[]).flatMap(b => (typeof b.text === 'string' ? [b.text] : [])).join('\n')
}

/** The same from a transcript file: a line that does not parse (one being written) is skipped. */
export function entriesOfFile(jsonl: string): PageEntry[] {
  const messages: Message[] = []
  const uses = new Map<string, Use>()
  for (const line of jsonl.split('\n')) {
    const queued = line.includes('"queued_command"')
    if (!queued && !line.includes('"message"')) continue
    let v: { type?: unknown; isMeta?: unknown; origin?: { kind?: unknown }; message?: { content?: unknown }; attachment?: { type?: unknown; prompt?: unknown } }
    try {
      v = JSON.parse(line) as typeof v
    } catch {
      continue
    }
    // A line that reached a worker in the middle of its turn is kept as an attachment, in its place.
    if (v.type === 'attachment' && v.attachment?.type === 'queued_command' && typeof v.attachment.prompt === 'string') {
      messages.push({ role: 'user', text: v.attachment.prompt, toolUses: [] })
      continue
    }
    if (v.type !== 'user' && v.type !== 'assistant') continue
    if (v.isMeta === true && v.origin?.kind !== 'human' && v.origin?.kind !== 'coordinator') continue
    const content = v.message?.content
    const blocks = (Array.isArray(content) ? content : []) as FileBlock[]
    const text = typeof content === 'string' ? content : blocks.flatMap(b => (b.type === 'text' && typeof b.text === 'string' ? [b.text] : [])).join('\n')
    const toolUses: Use[] = []
    for (const b of blocks) {
      if (b.type === 'tool_use' && typeof b.name === 'string') {
        const use: Use = { tool: b.name, input: (b.input ?? {}) as Record<string, unknown> }
        toolUses.push(use)
        if (typeof b.id === 'string') uses.set(b.id, use)
      } else if (b.type === 'tool_result' && typeof b.tool_use_id === 'string') {
        const use = uses.get(b.tool_use_id)
        if (!use) continue
        use.text = resultText(b.content)
        if (b.is_error === true) use.isError = true
      }
    }
    messages.push({ role: v.type, text, toolUses })
  }
  return entriesOf(messages)
}

/** The newest entries that fit the room, and how many older ones do not. */
export function inRoom(entries: readonly PageEntry[], room = ROOM): { shown: PageEntry[]; left: number } {
  let used = 0
  let from = entries.length
  while (from > 0 && used + entries[from - 1]!.text.length <= room) {
    from -= 1
    used += entries[from]!.text.length
  }
  return { shown: entries.slice(from), left: from }
}

/** About how tall a page draws at `columns`, its header and gaps with it: enough to tell a short page from a long one. */
export function pageRows(entries: readonly PageEntry[], columns: number): number {
  const HEADER = 5
  const across = Math.max(1, columns - 2)
  return entries.reduce((rows, e) => rows + 1 + (e.kind === 'tool' ? 1 : e.text.split('\n').reduce((n, line) => n + Math.max(1, Math.ceil(line.length / across)), 0)), HEADER)
}

/**
 * What a working worker is at, for the page's last line: `what` where the row has something to say
 * beyond its last tool call (a limit, a pause, a wait), else the call with no result yet, else thinking.
 */
export function doing(entries: readonly PageEntry[], what: string): string {
  if (what !== '' && what !== '"' && !entries.some(e => e.kind === 'tool' && e.text === what)) return what
  const last = entries[entries.length - 1]
  return last?.kind === 'tool' && last.result === undefined ? last.text : 'Thinking'
}

const DROP_WORDS = 'Prompt dropped by a hook: '

/** The blocks of the row Claude Code keeps for the drop with this reason, in the reason's words; undefined: not that row. */
export function ownDropBlocks(content: unknown, reason: string): { type: 'text'; text: string }[] | undefined {
  const blocks = Array.isArray(content) ? (content as { text?: unknown }[]) : []
  return blocks.some(b => b.text === `${DROP_WORDS}${reason}`) ? [{ type: 'text', text: reason }] : undefined
}

/** A Next up row's page with no brief: what it has, then how a brief is added. */
export function noBrief(key: string, title: string, note: string | undefined): string {
  const parts = [key && `Task: ${key}`, title !== key ? title : '', note?.trim() ?? ''].filter(Boolean)
  parts.push(`No brief was given for this row. The dispatcher adds one with \`giverny-manage plan ${key || '<task>'} --eta <min> --brief FILE\` (or \`start … --brief FILE\`).`)
  return parts.join('\n\n')
}

/** What the box says it does, by who a line goes to. */
export const askLine = (ask: 'worker' | 'start') =>
  ask === 'worker' ? 'Message this worker.' : "Add to this task's brief. Its worker reads it when the task starts."

/** What the start button on a Next up row's page gives the manager, who claims and spawns. */
export const startLine = (key: string) => `Start ${key} now.`

/** A brief with the person's line at its end, in the words the manage skill gives a worker. */
export const briefWith = (brief: string, text: string) => `${brief.trimEnd()}\n\nThe user adds: ${text}\n`

/** What the manager reads at its next turn of a line added to a Next up row: no turn is spent on it. */
export const briefNote = (key: string, file: string | undefined, text: string) =>
  `The user added this to task ${key}'s brief${file === undefined ? ' (it has no brief file)' : `, in ${file}`}: ${text}\n\nTheir words go to its worker when you start it, as theirs (\`The user adds: ...\`). This does not start the task or change when it starts.`

/**
 * The lines sent and not yet in the transcript: each prompt that lands takes the place of the
 * oldest, first in first out, the text not compared, as the manager may change it.
 */
export function pendingAfter(pending: readonly string[], promptsAtSend: number, entries: readonly PageEntry[]): string[] {
  const landed = entries.filter(e => e.kind === 'prompt').length - promptsAtSend
  return landed > 0 ? pending.slice(landed) : [...pending]
}
