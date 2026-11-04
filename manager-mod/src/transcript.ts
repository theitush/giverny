import type { Stop } from '../types'

// What a worker's transcript says that `$.agent.list()` does not: when it started and last
// wrote, what it does now, its context, and the spans it stood on an API error.
// crates/claude/src/subagents.rs fold_line, stop_reason, describe_tool_use, context_tokens.

export type TranscriptFacts = {
  lastMs?: number
  lastTurnMs?: number
  tokens?: number
  activity?: string
  model?: string
  stops: Stop[]
}

const ACTIVITY_ARGS = ['description', 'command', 'file_path', 'path', 'pattern', 'url', 'query', 'skill', 'to', 'prompt']
const DOING_MAX = 120

const stamp = (v: unknown) => {
  if (typeof v !== 'string') return undefined
  const n = Date.parse(v)
  return Number.isNaN(n) ? undefined : n
}

/** The timestamp of a transcript's first line. */
export function firstStamp(line: string): number | undefined {
  try {
    return stamp((JSON.parse(line) as { timestamp?: unknown }).timestamp)
  } catch {
    return undefined
  }
}

export function stopReason(line: { error?: unknown; message?: { content?: unknown } }): string {
  const content = line.message?.content
  const text = (Array.isArray(content) ? content : [])
    .flatMap(c => {
      const t = (c as { text?: unknown }).text
      return typeof t === 'string' ? [t] : []
    })
    .join(' ')
  const has = (...needles: string[]) => needles.some(n => text.includes(n))
  if (line.error === 'rate_limit') return 'usage limit'
  if (line.error === 'authentication_failed') return 'login expired'
  if (has('EAI_AGAIN', 'ENOTFOUND', 'ECONNREFUSED', 'ECONNRESET', "Can't reach", 'Connection lost', 'Connection error')) return 'no network'
  if (has('timed out', 'Timed out', 'timeout')) return 'timed out'
  if (has('limit')) return 'usage limit'
  if (has('/login', 'authenticate')) return 'login expired'
  return 'API error'
}

export function describeToolUse(use: { name?: unknown; input?: unknown }): string {
  const name = typeof use.name === 'string' ? use.name : 'tool'
  const input = (typeof use.input === 'object' && use.input !== null ? use.input : {}) as Record<string, unknown>
  const arg = ACTIVITY_ARGS.map(k => input[k]).find((v): v is string => typeof v === 'string')
  const flat = (arg === undefined ? name : `${name}: ${arg}`).split(/\s+/).join(' ')
  return [...flat].length > DOING_MAX ? `${[...flat].slice(0, DOING_MAX - 1).join('')}…` : flat
}

function contextTokens(u: Record<string, unknown>): number {
  const n = (v: Record<string, unknown>, k: string) => (typeof v[k] === 'number' ? (v[k] as number) : 0)
  const of = (v: Record<string, unknown>) => n(v, 'input_tokens') + n(v, 'cache_creation_input_tokens') + n(v, 'cache_read_input_tokens')
  const top = of(u)
  if (top === 0 || !Array.isArray(u.iterations)) return top
  const its = u.iterations as Record<string, unknown>[]
  const last = [...its].reverse().find(it => typeof it === 'object' && it !== null && it.type !== 'advisor_message' && it.type !== 'compaction')
  if (!last || (last.type !== 'message' && last.type !== 'fallback_message')) return top
  return of(last) || top
}

/** The facts in `text`, a transcript or its tail: a first line cut in half is skipped. */
export function foldTranscript(text: string): TranscriptFacts {
  const facts: TranscriptFacts = { stops: [] }
  for (const line of text.split('\n')) {
    if (line === '') continue
    const quick = /"timestamp":"([^"]+)"/.exec(line)?.[1]
    const at = stamp(quick)
    if (at !== undefined) facts.lastMs = at
    if (!line.includes('"assistant"')) continue
    let v: { type?: unknown; timestamp?: unknown; isApiErrorMessage?: unknown; error?: unknown; message?: Record<string, unknown> }
    try {
      v = JSON.parse(line) as typeof v
    } catch {
      continue
    }
    if (v.type !== 'assistant') continue
    const ts = stamp(v.timestamp)
    if (ts !== undefined) facts.lastTurnMs = ts
    const msg = v.message
    if (!msg) continue
    const open = facts.stops.at(-1)?.toMs === undefined ? facts.stops.at(-1) : undefined
    if (msg.model === '<synthetic>') {
      if (v.isApiErrorMessage === true && !open && ts !== undefined) facts.stops.push({ fromMs: ts, reason: stopReason(v as never) })
      continue
    }
    if (open) open.toMs = Math.max(ts ?? open.fromMs, open.fromMs)
    if (typeof msg.model === 'string') facts.model = msg.model
    if (typeof msg.usage === 'object' && msg.usage !== null) {
      const ctx = contextTokens(msg.usage as Record<string, unknown>)
      if (ctx > 0) facts.tokens = ctx
    }
    if (Array.isArray(msg.content)) {
      const use = [...msg.content].reverse().find(c => (c as { type?: unknown }).type === 'tool_use')
      if (use) facts.activity = describeToolUse(use as never)
    }
  }
  return facts
}

/** Spans kept from earlier reads, with what a later read of the tail found. */
export function mergeStops(known: Stop[], found: Stop[]): Stop[] {
  const out = known.map(s => ({ ...s }))
  for (const f of found) {
    const same = out.find(s => s.fromMs === f.fromMs)
    if (!same) out.push({ ...f })
    else if (same.toMs === undefined) same.toMs = f.toMs
  }
  return out.sort((a, b) => a.fromMs - b.fromMs).slice(-32)
}
