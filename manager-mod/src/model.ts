import type { AgentRow, DisplayRow, Feed, FeedRow, Model, Section } from '../types'

import { dur, elapsed, left } from './time'

// Feed rows and live agents into what the band draws: plain data, no `$`.
// join() is the doc's Merge rules 2 to 4, as crates/claude/src/feed.rs merge_with has them.

/** A feed row and the worker behind it, or a worker no feed row carries. */
export type Joined = { row?: FeedRow; agent?: AgentRow; finished?: boolean }

const WORD = /[\p{L}\p{N}_-]/u

const isRunning = (a: AgentRow) => ['pending', 'running', 'waiting', 'idle'].includes(a.status)

/** Does `text` name `key` as a whole word? `demo#82`, never `demo#820` or `xdemo#82`. */
export function namesKey(text: string, key: string): boolean {
  if (key === '') return false
  for (let i = text.indexOf(key); i !== -1; i = text.indexOf(key, i + 1)) {
    const before = [...text.slice(0, i)].pop()
    const after = [...text.slice(i + key.length)][0]
    if ((before === undefined || (!WORD.test(before) && before !== '#')) && (after === undefined || !WORD.test(after))) return true
  }
  return false
}

export function join(rows: FeedRow[], agents: AgentRow[]): Joined[] {
  // An agent a listed agent spawned works for its parent, whose task it may well name: only
  // an agent_id makes it a row's worker (rule 10 hangs it under its parent).
  const nested = (a: AgentRow) => a.parentId !== undefined && agents.some(o => o.id === a.parentId)
  const names = (a: AgentRow, key: string) => !nested(a) && namesKey(a.description, key)
  // A Running row wants a worker still running, any other a finished one; either takes the other.
  const described = (r: FeedRow) => {
    const named = agents.filter(a => names(a, r.key))
    return named.find(a => isRunning(a) === (r.stage === 'running')) ?? named[0]
  }
  // A Running row whose worker ended, and a later worker naming the task runs: it took over.
  const successor = (r: FeedRow, held: AgentRow) =>
    agents.find(a => isRunning(a) && a.id !== held.id && names(a, r.key) && !rows.some(o => o.agent_id === a.id))
  const out: Joined[] = rows.map(row => {
    const held = agents.find(a => a.id === row.agent_id)
    let agent = held ?? described(row)
    if (held && row.stage === 'running' && (!isRunning(held) || row.killed !== undefined)) agent = successor(row, held) ?? held
    return { row, agent, finished: row.follows_worker === true && agent !== undefined && !isRunning(agent) }
  })
  for (const agent of agents) {
    if (out.some(j => j.agent?.id === agent.id)) continue
    const task = rows.find(r => r.agent_id === agent.id || names(agent, r.key))
    // Running and naming a task: that row's worker, never drawn twice. Finished: it was
    // replaced on the task, and keeps a Done row of its own (giverny#513).
    if (task && isRunning(agent) && task.killed === undefined) continue
    out.push({ agent })
  }
  return out
}

/** What a Next up row waits for. */
export function waitsOn(r: FeedRow): string {
  if (r.held) return `held: ${r.held}`
  if (r.lease?.state === 'queued') return 'queued for resources'
  if (r.after) return `after ${r.after}`
  return ''
}

function fromFeed(r: FeedRow, agent: AgentRow | undefined, section: Section, now: number): DisplayRow {
  const base = { id: r.key, section, key: r.key, title: r.title ?? agent?.description ?? '', workerId: agent?.id, over: false }
  if (section === 'running') {
    const l = left(r, now)
    return { ...base, elapsed: elapsed(r, now), left: l, over: l.startsWith('over'), what: r.note ?? '' }
  }
  if (section === 'next') {
    return { ...base, elapsed: '', left: r.eta_s === undefined ? '' : `~${dur(r.eta_s)}`, what: waitsOn(r) }
  }
  const est = r.eta_first_s ?? r.eta_s
  const took = r.started !== undefined && r.ended !== undefined ? dur((r.ended - r.started) / 1000) : ''
  return { ...base, elapsed: took, left: est === undefined ? '' : `est ${dur(est)}`, what: r.landing ?? '' }
}

const fromAgent = (a: AgentRow, section: Section): DisplayRow => ({
  id: a.id,
  section,
  key: '',
  title: a.description,
  elapsed: '',
  left: '',
  over: false,
  what: section === 'done' ? a.status : '',
  workerId: a.id,
})

const SECTION: Record<FeedRow['stage'], Section> = { running: 'running', planned: 'next', done: 'done' }

/** The band's rows: Running and Next up in feed order, then Done, newest landed first. */
export function buildModel(feed: Feed | undefined, agents: AgentRow[], now: number): Model {
  const drawn = join(feed?.rows ?? [], agents).flatMap(({ row, agent, finished }): { section: Section; shown: DisplayRow; ended: number }[] => {
    if (row) {
      const section = finished ? 'done' : SECTION[row.stage]
      return [{ section, shown: fromFeed(row, agent, section, now), ended: row.ended ?? -1 }]
    }
    if (!agent) return []
    const section = isRunning(agent) ? 'running' : 'done'
    return [{ section, shown: fromAgent(agent, section), ended: -1 }]
  })
  const of = (section: Section) => drawn.filter(d => d.section === section)
  const done = of('done').sort((a, b) => b.ended - a.ended)
  const running = of('running')
  const next = of('next')
  return {
    rows: [...running, ...next, ...done].map(d => d.shown),
    running: running.length,
    next: next.length,
    done: done.length,
  }
}
