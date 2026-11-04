import type { PageEntry } from '../types'

import { describeToolUse } from './transcript'

// What a worker's page lists, plain data with no `$`: from `$.session.messages({ agentId })`,
// or from its transcript file where the session cannot read it (a worker of an earlier run).
// As crates/claude/src/transcript.rs draws it: the prompt, the words, one line per tool call.

type Use = { tool: string; input: Record<string, unknown>; isError?: true; text?: string }
type Message = { role: 'user' | 'assistant'; text: string; toolUses: readonly Use[] }

/** The room a page's texts have: a tree draws its first 100,000 characters and cuts the rest. */
export const ROOM = 80_000

// How Claude Code wraps a line that reaches a worker in the middle of its turn.
const WRAPPERS: [string, RegExp][] = [
  ['The coordinator sent a message while you were working:\n', /\n\nAddress this before completing your current task\.$/],
  ['The user sent a new message while you were working:\n', /\n\nThis is how Claude Code surfaces messages the user sends mid-turn[^]*$/],
]

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
      out.push({ kind: 'tool', text: describeToolUse({ name: use.tool, input: use.input }), isError: use.isError })
    }
  }
  return out
}

/** The same from a transcript file: a line that does not parse (one being written) is skipped. */
export function entriesOfFile(jsonl: string): PageEntry[] {
  const messages: Message[] = []
  for (const line of jsonl.split('\n')) {
    if (!line.includes('"message"')) continue
    let v: { type?: unknown; isMeta?: unknown; origin?: { kind?: unknown }; message?: { content?: unknown } }
    try {
      v = JSON.parse(line) as typeof v
    } catch {
      continue
    }
    if (v.type !== 'user' && v.type !== 'assistant') continue
    if (v.isMeta === true && v.origin?.kind !== 'human' && v.origin?.kind !== 'coordinator') continue
    const content = v.message?.content
    const blocks = (Array.isArray(content) ? content : []) as { type?: unknown; text?: unknown; name?: unknown; input?: unknown }[]
    const text = typeof content === 'string' ? content : blocks.flatMap(b => (b.type === 'text' && typeof b.text === 'string' ? [b.text] : [])).join('\n')
    const toolUses = blocks.flatMap(b => (b.type === 'tool_use' && typeof b.name === 'string' ? [{ tool: b.name, input: (b.input ?? {}) as Record<string, unknown> }] : []))
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

/** A Next up row's page with no brief: what it has, then how a brief is added. */
export function noBrief(key: string, title: string, note: string | undefined): string {
  const parts = [key && `Task: ${key}`, title !== key ? title : '', note?.trim() ?? ''].filter(Boolean)
  parts.push(`No brief was given for this row. The dispatcher adds one with \`giverny-manage plan ${key || '<task>'} --eta <min> --brief FILE\` (or \`start … --brief FILE\`).`)
  return parts.join('\n\n')
}

/** What the box says it does, by who a line goes to. */
export const askLine = (ask: 'worker' | 'start') =>
  ask === 'worker' ? 'Message this worker.' : 'Start this task now. Its manager adds this to its brief.'

/** The line a Next up row's page gives the manager, as Giverny's box does. */
export const startLine = (key: string, text: string) => `Start ${key} now, adding this to its brief: ${text}`

/**
 * The lines sent and not yet in the transcript: each prompt that lands takes the place of the
 * oldest, first in first out, the text not compared, as the manager may change it.
 */
export function pendingAfter(pending: readonly string[], promptsAtSend: number, entries: readonly PageEntry[]): string[] {
  const landed = entries.filter(e => e.kind === 'prompt').length - promptsAtSend
  return landed > 0 ? pending.slice(landed) : [...pending]
}
