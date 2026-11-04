import type { AgentRow, Around, DisplayRow, DoneRows, Feed, FeedRow, Model, RowFold, RowUse, Section, Shell, Stage, Stop } from '../types'

import { GRANTED_WAITING, leaseFact, queuedNote, queuedWhy, usageFact, withLedger } from './lease'
import { gist, named } from './shell-name'
import { loadOf, memShare, plusUse } from './stats'
import { clockAt, countdown, fmtDelta, fmtSpan, fmtTokens, offMs } from './time'

// Feed rows and live agents into what the band draws: plain data, no `$`.
// docs/management-panel.md **Merge with the live rows**, as crates/claude/src/feed.rs
// (merge_with, queue, PaneRow) and crates/app/src/management_panel.rs (bench, planned_wait,
// start_order, nest, format_row, dittos, chores) really do it. The names follow theirs.

const MIN = 60_000
/** How long after its worker a row must have begun to be a later task of that worker. */
const LATER_TASK_MS = 5 * MIN
/** How long after a Running row started, or was handed on, its worker may take to show up. */
const HANDOFF_WINDOW_MS = 3 * MIN
const IDLE_PLANNED_MS = 5 * MIN
const LAND_GRACE_MS = 3 * MIN
const NEST_W = 3

export const NO_ETA = 'no ETA'
export const NO_WORKER_YET = 'no worker yet'
export const PAUSED_FINISHED = 'paused · worker finished'
export const PAUSED_FINISHING = 'paused · worker still finishing'
export const KILLED = 'killed'
const IDLE_WHY = 'not started — waits on nothing; its manager is to start it'
const WRITER = 'giverny/manage'

const WORD = /[\p{L}\p{N}_-]/u

export const isRunning = (a: AgentRow) => ['pending', 'running', 'waiting', 'idle'].includes(a.status)

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

const lastSign = (l: AgentRow) => {
  const signs = [l.lastTurnMs, l.startedMs].filter((t): t is number => t !== undefined)
  return signs.length > 0 ? Math.max(...signs) : undefined
}

/** Did worker `l` of row `f` die with its session: a tracker may still list such a worker running. */
function diedWithSession(f: FeedRow, l: AgentRow): boolean {
  if (f.killed !== undefined) return true
  if (f.went_off === undefined) return false
  const sign = lastSign(l)
  return sign === undefined || sign <= f.went_off
}

/** A feed row and the worker behind it, or a worker no feed row carries. */
export type Joined = { row?: FeedRow; agent?: AgentRow; finished?: boolean; killed?: boolean }

/** Rules 2 to 4: who stands behind each row, and which workers stand alone. */
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
  // A Running row whose worker ended or died, and a later worker naming the task runs: it took over.
  const successor = (r: FeedRow, held: AgentRow) =>
    agents.find(
      a =>
        isRunning(a) &&
        a.id !== held.id &&
        names(a, r.key) &&
        !rows.some(o => o.agent_id === a.id) &&
        (a.startedMs === undefined || held.startedMs === undefined || a.startedMs > held.startedMs),
    )
  const out: Joined[] = rows.map(row => {
    const held = agents.find(a => a.id === row.agent_id)
    let agent = held ?? described(row)
    if (held && row.stage === 'running' && (!isRunning(held) || diedWithSession(row, held))) agent = successor(row, held) ?? held
    return { row, agent, finished: row.follows_worker === true && agent !== undefined && !isRunning(agent) }
  })
  for (const agent of agents) {
    if (out.some(j => j.agent?.id === agent.id)) continue
    const tasks = rows.filter(r => r.agent_id === agent.id || names(agent, r.key))
    // Running and naming a task: that row's worker, never drawn twice. Finished or killed: it
    // was replaced on the task, and keeps a Done row of its own (giverny#513).
    const killed = isRunning(agent) && tasks.some(f => diedWithSession(f, agent))
    if (tasks.length > 0 && isRunning(agent) && !killed) continue
    out.push({ agent, killed })
  }
  return out
}

/** One row of the pane: a feed row, a live row, or both joined. */
type PaneRow = {
  stage: Stage
  feed?: FeedRow
  live?: AgentRow
  ditto: boolean
  /** The start of the same worker's next task, and its key: this task ended there at the latest. */
  nextMs?: number
  nextKey?: string
  /** One of a worker's tasks taken in turn, finished: its own count, never the worker's whole one. */
  splitTask: boolean
  /** A Planned row queued on a worker busy with another task: that task. */
  afterKey?: string
  batch: number
}

const agentIdOf = (r: PaneRow) => r.feed?.agent_id ?? r.live?.id

/** How far a writer moved a row's `started` on for its pauses. */
const movedMs = (f: FeedRow) => (f.spawned !== undefined && f.started !== undefined ? Math.max(0, f.started - f.spawned) : (f.paused_s ?? 0) * 1000)

/** The task the spawn named: the worker's description names its key. */
const spawnTask = (r: PaneRow) => r.feed !== undefined && r.live !== undefined && namesKey(r.live.description, r.feed.key)

/** The start of the row's clock (rule 2). */
function startedOf(r: PaneRow): number | undefined {
  const worker = r.live?.startedMs
  const f = r.feed
  if (!f) return worker
  const moved = movedMs(f)
  if (worker === undefined) return f.started
  if (f.started === undefined) return worker
  const began = Math.max(0, f.started - moved)
  if ((began > worker + LATER_TASK_MS && !spawnTask(r)) || worker > began + LATER_TASK_MS) return f.started
  return worker + moved
}

/** When this task was handed to a worker already there; none for the task the spawn named. */
function handedAt(r: PaneRow): number | undefined {
  if (spawnTask(r) || !r.feed || r.feed.started === undefined) return undefined
  return Math.max(0, r.feed.started - movedMs(r.feed))
}

function endedOf(r: PaneRow): number | undefined {
  const own = r.feed?.ended ?? (r.live && !isRunning(r.live) ? r.live.endedMs : undefined)
  if (own !== undefined && r.nextMs !== undefined) return Math.min(own, r.nextMs)
  return own ?? r.nextMs
}

function tokensOf(r: PaneRow): number | undefined {
  if (r.stage === 'done' && r.feed?.task_tokens !== undefined) return r.feed.task_tokens
  // Tasks in turn: a Done row's own count wants the worker's whole transcript. Blank, never
  // the worker's whole count again (rule 7).
  if (r.splitTask) return undefined
  if (r.stage === 'planned') return undefined
  return r.live?.tokens ?? r.feed?.tokens
}

const lastFigure = (r: PaneRow): [number, number] | undefined => {
  const f = r.feed
  if (!f || f.left_at === undefined || f.left_s === undefined) return undefined
  const s = startedOf(r)
  return [s === undefined ? f.left_at : Math.max(f.left_at, s), f.left_s]
}

/** A Done row's ± in seconds, and the words for what it was measured against. */
function doneDelta(workS: number, reest: [number, number] | undefined, estimate: number | undefined): [number, string] | undefined {
  if (reest) return [Math.max(0, workS - reest[1]) - reest[0], `worker's ${fmtSpan(reest[0])}`]
  if (estimate === undefined) return undefined
  return [workS - estimate, `estimate ${fmtSpan(estimate)}`]
}

function etaDelta(r: PaneRow, off: (a: number, b: number) => number): [number, string] | undefined {
  const f = r.feed
  if (r.stage !== 'done' || !f) return undefined
  if (f.eta_delta_s !== undefined) return [f.eta_delta_s, 'estimate']
  const [start, end] = [startedOf(r), endedOf(r)]
  if (start === undefined || end === undefined) return undefined
  const clock = Math.floor(Math.max(0, end - start - off(start, end)) / 1000)
  const workS = Math.max(0, clock - (f.wait_s ?? 0))
  const reest: [number, number] | undefined = f.reest_s !== undefined && f.reest_at_s !== undefined ? [f.reest_s, f.reest_at_s] : undefined
  return doneDelta(workS, reest, f.eta_guess_s ?? f.eta_first_s ?? f.eta_s)
}

/** Rule 7: one worker, tasks one after another. */
function queueTasks(rows: PaneRow[]): void {
  const ids = [...new Set(rows.flatMap(r => agentIdOf(r) ?? []))]
  for (const id of ids) {
    const held = rows
      .map((r, i) => ({ r, i, s: startedOf(r) }))
      .filter(x => x.r.stage !== 'planned' && agentIdOf(x.r) === id && x.s !== undefined)
      .sort((a, b) => (a.s as number) - (b.s as number) || a.i - b.i)
    const batches: { start: number; rows: PaneRow[] }[] = []
    for (const { r, s } of held) {
      const last = batches.at(-1)
      if (last && (s as number) < last.start + LATER_TASK_MS) last.rows.push(r)
      else batches.push({ start: s as number, rows: [r] })
    }
    if (batches.length >= 2) {
      batches.forEach((batch, k) => {
        const next = batches[k + 1]
        for (const r of batch.rows) {
          r.batch = k
          if (next) {
            r.nextMs = next.start
            r.nextKey = next.rows[0]?.feed?.key || undefined
            r.stage = 'done'
          }
          if (r.stage === 'done') r.splitTask = true
        }
      })
    }
    const running = rows.find(r => r.stage === 'running' && agentIdOf(r) === id)?.feed?.key
    if (running) {
      for (const r of rows) {
        if (r.stage === 'planned' && r.feed?.agent_id === id) r.afterKey = running
      }
    }
  }
}

const STAGE_RANK: Record<Stage, number> = { running: 0, planned: 1, done: 2 }

/** Rule 5's base order and rule 6: Running and Next up as they came, Done newest landed first. */
function merge(feedRows: FeedRow[], agents: AgentRow[]): PaneRow[] {
  const rows = join(feedRows, agents).flatMap((j): PaneRow[] => {
    const bare = { ditto: false, splitTask: false, batch: 0 }
    if (j.row) return [{ ...bare, stage: j.finished ? 'done' : j.row.stage, feed: j.row, live: j.agent }]
    if (!j.agent) return []
    const done = j.killed === true || !isRunning(j.agent)
    return [{ ...bare, stage: done ? 'done' : 'running', live: j.agent, nextMs: j.killed ? lastSign(j.agent) : undefined }]
  })
  queueTasks(rows)
  const groupEnd = (r: PaneRow) => {
    const id = agentIdOf(r)
    if (id === undefined) return undefined
    const ends = rows.filter(o => o.stage === 'done' && agentIdOf(o) === id && o.batch === r.batch).flatMap(o => endedOf(o) ?? [])
    return ends.length > 0 ? Math.max(...ends) : undefined
  }
  const keyed = rows.map((r, i) => ({ r, i, end: r.stage === 'done' ? (groupEnd(r) ?? endedOf(r)) : undefined }))
  keyed.sort((a, b) => {
    const stage = STAGE_RANK[a.r.stage] - STAGE_RANK[b.r.stage]
    if (stage !== 0) return stage
    if (a.end === undefined || b.end === undefined) return a.end === b.end ? a.i - b.i : a.end === undefined ? 1 : -1
    return b.end - a.end || a.i - b.i
  })
  const out = keyed.map(k => k.r)
  reditto(out)
  return out
}

/** A row is a ditto of the one directly above it: the same worker's same batch. */
function reditto(rows: PaneRow[]): void {
  rows.forEach((r, i) => {
    const above = rows[i - 1]
    const [a, b] = [above && agentIdOf(above), agentIdOf(r)]
    r.ditto = above !== undefined && a !== undefined && a === b && above.batch === r.batch
  })
}

/** A Running row whose worker was killed with its session: it reads `killed` until revived. */
function killed(r: PaneRow): boolean {
  const f = r.feed
  if (!f || r.stage !== 'running') return false
  return (f.killed !== undefined && f.paused_since !== undefined) || (r.live !== undefined && diedWithSession(f, r.live))
}

/** Why nothing runs a Running row. */
type Unworked = { kind: 'stopped'; reason: string } | { kind: 'finished'; how: string; endedMs?: number } | { kind: 'no-worker'; since: number }

const HOW: Record<string, string> = { failed: 'failed', killed: 'killed', stopped: 'stopped' }

function unworked(r: PaneRow, now: number): Unworked | undefined {
  const l = r.live
  if (l) {
    if (isRunning(l)) return undefined
    const endedMs = l.endedMs ?? l.lastTurnMs
    const handed = handedAt(r)
    // An idle worker handed this task and not woken yet ended its previous task, not this one.
    if (endedMs !== undefined && handed !== undefined && endedMs < handed) {
      return now < handed + HANDOFF_WINDOW_MS ? undefined : { kind: 'no-worker', since: handed }
    }
    const stop = l.stops?.at(-1)
    if (stop && stop.toMs === undefined) return { kind: 'stopped', reason: stop.reason }
    return { kind: 'finished', how: HOW[l.status] ?? 'finished', endedMs }
  }
  const f = r.feed
  if (!f || f.started === undefined) return undefined
  return now >= f.started + HANDOFF_WINDOW_MS && !f.live ? { kind: 'no-worker', since: f.started } : undefined
}

/** Rule 9: a Running row nobody works on is drawn under Next up. */
function benched(r: PaneRow, now: number): boolean {
  const f = r.feed
  if (!f || r.stage !== 'running' || killed(r)) return false
  const setAside = f.held !== undefined || f.paused_since !== undefined
  const u = unworked(r, now)
  if (!u) return setAside && !f.live && (r.live === undefined || !isRunning(r.live))
  if (u.kind === 'no-worker') return true
  return u.kind === 'finished' && u.how === 'finished' && setAside
}

function bench(rows: PaneRow[], now: number): PaneRow[] {
  let moved = false
  for (const r of rows) {
    if (benched(r, now)) {
      r.stage = 'planned'
      moved = true
    }
  }
  if (!moved) return rows
  const out = rows.map((r, i) => ({ r, i })).sort((a, b) => STAGE_RANK[a.r.stage] - STAGE_RANK[b.r.stage] || a.i - b.i).map(x => x.r)
  reditto(out)
  return out
}

/** Where a Next up row stands in line to start: `[rank, place]`, first to last. */
type Start = [number, number]
const READY: Start = [0, 0]
const queued = (place: number): Start => [1, place]
const IDLE: Start = [2, 0]
const AFTER: Start = [3, 0]
const SET_ASIDE: Start = [4, 0]
const startCmp = (a: Start, b: Start) => a[0] - b[0] || a[1] - b[1]

/** What a Next up row's NOW says (rule 8). */
type Wait = { note: string; why: string; start: Start; held?: string; after?: string; idleSince?: number }

function plannedWait(r: PaneRow, rows: PaneRow[]): Wait | undefined {
  if (r.stage !== 'planned') return undefined
  const f = r.feed
  if (f?.held !== undefined) return { note: `held: ${f.held}`, why: `held — not started; waiting for: ${f.held}`, held: f.held, start: SET_ASIDE }
  if (f?.stage === 'running') {
    return {
      note: NO_WORKER_YET,
      why: `${NO_WORKER_YET} — started, but no worker has picked it up`,
      start: f.paused_since !== undefined ? SET_ASIDE : READY,
    }
  }
  const lease = f?.lease
  if (lease?.state === 'queued') return { note: queuedNote(lease), why: queuedWhy(lease), start: queued(lease.position ?? Number.MAX_SAFE_INTEGER) }
  if (lease?.uncollected) return { note: GRANTED_WAITING, why: `${GRANTED_WAITING} — its lease is granted; its manager is to start it`, start: READY }
  // The task it was planned after: waited for until it is drawn Done; one gone from the feed is no wait.
  const before = f?.after === undefined ? undefined : rows.find(o => o.feed?.key === f.after)
  if (before && before.stage !== 'done') {
    const k = before.feed?.key ?? ''
    return { note: `after ${k}`, why: `after ${k} — starts when that lands`, start: AFTER, after: k }
  }
  if (r.afterKey !== undefined) {
    const k = r.afterKey
    return { note: `after ${k}`, why: `after ${k} — starts when its worker is done with that`, start: AFTER, after: k }
  }
  if (!f) return undefined
  const beforeEnd = before ? endedOf(before) : undefined
  const instants = [f.planned, f.unheld, beforeEnd, lease?.granted_ms].filter((t): t is number => t !== undefined)
  return { note: '', why: IDLE_WHY, start: lease ? READY : IDLE, idleSince: instants.length > 0 ? Math.max(...instants) : undefined }
}

/** Rule 5: Next up reads in the order its rows will start, a row after another below it. */
function startOrder(rows: PaneRow[], waits: (Wait | undefined)[]): { stage: number; start: Start; depth: number }[] {
  const place = (i: number, seen: number[]): { start: Start; depth: number } => {
    const w = waits[i]
    if (!w) return { start: IDLE, depth: 0 }
    const j = w.after === undefined ? -1 : rows.findIndex(o => o.stage === 'planned' && o.feed?.key === w.after)
    // A loop of `after`s is no chain: its rows keep their own place.
    if (j !== -1 && j !== i && !seen.includes(j)) {
      seen.push(i)
      const up = place(j, seen)
      return { start: startCmp(up.start, w.start) >= 0 ? up.start : w.start, depth: up.depth + 1 }
    }
    return { start: w.start, depth: w.after !== undefined ? 1 : 0 }
  }
  return rows.map((r, i) => {
    const stage = STAGE_RANK[r.stage]
    return r.stage === 'planned' ? { stage, ...place(i, []) } : { stage, start: READY, depth: 0 }
  })
}

/** Rule 1: is this row drawn? */
function keeps(r: PaneRow, manager: boolean): boolean {
  if (r.live) return true
  if (!manager) return false
  const f = r.feed
  return r.stage === 'planned' || (f !== undefined && (f.lease !== undefined || f.usage !== undefined || f.live === true))
}

/** Rule 10: which row each agent hangs under, by index. */
function parentRows(rows: PaneRow[]): (number | undefined)[] {
  const up = rows.map((r, i): number | undefined => {
    const l = r.live
    if (!l || l.parentId === undefined) return undefined
    const holds = rows.flatMap((o, j) => (j !== i && o.stage !== 'planned' && agentIdOf(o) === l.parentId ? [j] : []))
    // Under the task that was under way when it spawned.
    let best: number | undefined
    for (const j of holds) {
      const [a, b] = [startedOf(rows[j] as PaneRow), l.startedMs]
      if (a === undefined || b === undefined || a > b) continue
      const seen = best === undefined ? undefined : startedOf(rows[best] as PaneRow)
      if (seen === undefined || a >= seen) best = j
    }
    return best ?? holds[0]
  })
  up.forEach((_, i) => {
    let at = up[i]
    let steps = 0
    while (at !== undefined) {
      if (at === i || steps > up.length) {
        up[i] = undefined
        break
      }
      at = up[at]
      steps += 1
    }
  })
  return up
}

const foldIdent = (r: PaneRow) => r.feed?.key || r.live?.id || agentIdOf(r) || ''
const displayName = (l: AgentRow) => l.name ?? (l.description || l.id)
const rowName = (r: PaneRow) => r.feed?.key || (r.live && displayName(r.live)) || ''

type Nested = { at: number; depth: number; fold?: RowFold; parent?: string }

/** Rule 10 and `done_rows`: the rows as the pane places them. */
function nest(rows: PaneRow[], done: DoneRows, folded: string[]): Nested[] {
  const n = rows.length
  const up = parentRows(rows)
  const level = (i: number) => {
    let d = 0
    for (let at = up[i]; at !== undefined; at = up[at]) d += 1
    return d
  }
  const dropped = rows.map(() => false)
  if (done !== 'all') {
    let seen = 0
    rows.forEach((r, i) => {
      if (r.stage !== 'done') return
      if (done === 'hide') dropped[i] = true
      else if (up[i] === undefined) {
        seen += 1
        dropped[i] = seen > done
      }
    })
    // Shallowest first, so a row's spawner is settled before it is. A running agent is never
    // hidden by the Done setting: it stands on its own instead.
    const levels = rows.map((_, i) => level(i))
    const byLevel = [...Array(n).keys()].sort((a, b) => (levels[a] as number) - (levels[b] as number))
    for (const i of byLevel) {
      const p = up[i]
      if (p !== undefined && dropped[p]) {
        if (rows[i]?.stage === 'done') dropped[i] = true
        else up[i] = undefined
      }
    }
  }
  const kids: number[][] = rows.map(() => [])
  rows.forEach((_, i) => {
    const p = up[i]
    if (!dropped[i] && p !== undefined) kids[p]?.push(i)
  })
  const under = (i: number): [number, number] =>
    (kids[i] ?? []).reduce<[number, number]>(
      ([all, run], k) => {
        const [a, r] = under(k)
        return [all + 1 + a, run + (rows[k]?.stage === 'running' ? 1 : 0) + r]
      },
      [0, 0],
    )
  const out: Nested[] = []
  const place = (i: number, depth: number, parent: string | undefined) => {
    const row = rows[i] as PaneRow
    let fold: RowFold | undefined
    if ((kids[i] ?? []).length > 0) {
      const [all, running] = under(i)
      const id = foldIdent(row)
      const isFolded = folded.includes(id)
      fold = {
        id,
        folded: isFolded,
        under: all,
        running,
        note: !isFolded ? '' : running === 0 ? `+${all}` : `+${all}, ${running} running`,
        shortNote: isFolded ? `+${all}` : '',
      }
    }
    out.push({ at: i, depth, fold, parent })
    if (!fold?.folded) for (const k of kids[i] ?? []) place(k, depth + 1, rowName(row))
  }
  rows.forEach((_, i) => {
    if (!dropped[i] && up[i] === undefined) place(i, 0, undefined)
  })
  return out
}

function foldWhy(f: RowFold): string {
  const agents = f.under === 1 ? 'agent' : 'agents'
  const running = f.running === 0 ? '' : `, ${f.running} running`
  return f.folded ? `${f.under} ${agents} of its own folded under it${running} — ▸ shows them` : `${f.under} ${agents} of its own under it${running} — ▾ folds them`
}

/** What a title is drawn behind. `nested`: some row has a toggle, so the titles stay aligned. */
function nestHead(fold: RowFold | undefined, depth: number, nested: boolean): string {
  const indent = ' '.repeat(NEST_W * depth)
  if (fold) return `${indent}${fold.folded ? '▸' : '▾'} `
  if (depth > 0) return `${indent}└ `
  return nested ? '  ' : ''
}

const stoppedMs = (l: AgentRow | undefined, start: number, upto: number) =>
  (l?.stops ?? []).reduce((sum, s: Stop) => sum + Math.max(0, Math.min(s.toMs ?? upto, upto) - Math.max(s.fromMs, start)), 0)

/** A worker standing on an API error, or ended badly: `[since, words]`. */
function stoppedAt(l: AgentRow): [number, string] | undefined {
  const s = l.stops?.at(-1)
  if (s && s.toMs === undefined) return [s.fromMs, `stopped: ${s.reason}`]
  if (isRunning(l)) return undefined
  const word = HOW[l.status]
  return word && l.endedMs !== undefined ? [l.endedMs, `stopped: ${word}`] : undefined
}

const SHORT_MODELS = ['haiku', 'sonnet', 'opus', 'fable']
const shortModel = (m: string | undefined) => SHORT_MODELS.find(f => m?.toLowerCase().includes(f))

/** The `agent-<id>` a worker with no task once started its own row under names nothing. */
const shownKey = (f: FeedRow) => (f.follows_worker && /^agent-[A-Za-z0-9]+$/.test(f.key) ? '' : f.key)

const stageSection: Record<Stage, Section> = { running: 'running', planned: 'next', done: 'done' }

const limitOut = (a: Around, now: number) => (a.limit && (a.limit.reopensMs === undefined || a.limit.reopensMs > now) ? a.limit : undefined)

export function limitNote(window: string | undefined, reopensMs: number | undefined, now: number): string {
  const what = window ? `${window} limit` : 'limit'
  return reopensMs === undefined ? what : `${what} → ${clockAt(reopensMs, now)}`
}

function asleepOn(r: PaneRow, a: Around, now: number): Shell[] | undefined {
  const l = r.live
  if (!l) return undefined
  const u = unworked(r, now)
  if (!u || u.kind !== 'finished' || u.how !== 'finished') return undefined
  const shells = a.shells[l.id]
  return shells && shells.length > 0 ? shells : undefined
}

type Waiting = { since: number; note?: string }

const waitSuffix = (w: Waiting | undefined) => {
  if (!w) return ''
  const n = w.note?.trim()
  return n ? ` · waiting: ${n}` : ' · waiting'
}

function asleepNow(shells: Shell[], w: Waiting | undefined, now: number): string {
  return `asleep · ${named(shells, now)} · wakes when ${shells.length === 1 ? 'it ends' : 'one ends'}${waitSuffix(w)}`
}

function asleepWhy(shells: Shell[], endedMs: number | undefined, now: number): string {
  const since = endedMs === undefined ? '' : ` since ${clockAt(endedMs, now)}`
  const list = shells.map(s => `\`${gist(s.cmd)}\` (${fmtSpan(Math.floor((now - s.startedMs) / 1000))} in, shell ${s.id})`).join(', ')
  const [what, when] = shells.length === 1 ? ['its own background shell runs', 'that ends'] : ['its own background shells run', 'one ends']
  return `its worker is asleep${since}: its turn ended while ${what}: ${list}. Claude Code wakes it when ${when}; nothing to land yet`
}

function turnEnded(endedMs: number | undefined, w: Waiting | undefined, now: number): string {
  return `${endedMs === undefined ? 'turn ended' : `turn ended ${clockAt(endedMs, now)}`}${waitSuffix(w)}`
}

function waitingWhy(w: Waiting, now: number): string {
  const since = clockAt(w.since, now)
  const n = w.note?.trim()
  return n ? `waiting since ${since}: ${n}` : `waiting since ${since}, on something that is not the work`
}

const pausedWhy = (since: string, note: string | undefined) => (note ? `paused since ${since}: ${note}` : `paused since ${since} — its clock is stopped`)

function killedWhy(r: PaneRow, now: number): string {
  const f = r.feed
  const at = f?.went_off ?? f?.paused_since
  const off = at === undefined ? '' : ` at ${clockAt(at, now)}`
  const clock =
    r.stage === 'running' && f?.paused_since !== undefined
      ? 'its clock stopped there until its manager revives it (a fresh worker, then resume)'
      : 'resumed with no worker: a fresh one whose description names the task takes the row'
  return `killed: Giverny, WSL or its manager session went off${off}, and its worker with it; ${clock}`
}

function runningWhy(who: string, since: string | undefined, estimate: number | undefined, leftS: number | undefined, state: string | undefined): string[] {
  const head = `Running: ${who}${since ? ` since ${since}` : ''}`
  let est = 'no estimate yet'
  if (estimate !== undefined) {
    if (leftS === undefined) est = `estimate ${fmtSpan(estimate)}`
    else if (leftS < 0 && fmtSpan(leftS) !== '0m') est = `estimate ${fmtSpan(estimate)}: ${fmtSpan(-leftS)} over it`
    else est = `estimate ${fmtSpan(estimate)}: ${countdown(leftS)} left, on track`
  }
  return [head, ...(state ? [state] : []), est]
}

function doneWhy(now: string, elapsed: string, delta: [number, string] | undefined, review: string | undefined): string[] {
  let head = 'Done'
  if (now !== '' && now.toLowerCase() !== 'done') head += `: ${now}`
  if (elapsed !== '') {
    head += `; took ${elapsed}`
    if (delta) {
      const what = `its ${delta[1]}`
      const s = fmtSpan(Math.abs(delta[0]))
      head += s === '0m' ? `, on ${what}` : delta[0] > 0 ? `, ${s} over ${what}` : `, ${s} under ${what}`
    } else head += ', no estimate'
  }
  return [head, ...(review ? [`awaiting review: ${review}`] : [])]
}

const noEtaHint = (key: string, agentId: string | undefined) =>
  key ? `no ETA — add one: giverny-manage eta ${key} <min> --why scope` : `no ETA — add one: giverny-eta ${agentId ?? '<agent-id>'} <min>`

const OUTCOME: Record<string, string> = { failed: 'failed', killed: 'killed', stopped: 'stopped' }

/** format_row: one row's cells, before any ditto. */
function formatRow(r: PaneRow, now: number, written: number | undefined, a: Around, wait: Wait | undefined, nested: Nested, anyFold: boolean): DisplayRow {
  const f = r.feed
  const l = r.live
  const key = f?.key ?? ''
  let id = ''
  let title = ''
  if (f) {
    id = shownKey(f) || shortModel(l?.model) || ''
    title = f.title ?? (l ? displayName(l) : '')
  } else if (l) {
    if (l.name && l.description) {
      id = l.name
      title = l.description
    } else {
      id = shortModel(l.model) ?? ''
      title = displayName(l)
    }
  }
  const rowStart = startedOf(r)
  const paused = r.stage === 'running' ? f?.paused_since : undefined
  const benchedPause = r.stage === 'planned' && f?.stage === 'running' ? f.paused_since : undefined
  // A row that carries its pauses has its stops in `started` already; the pane's holds would count twice.
  const writerPauses = f !== undefined && (f.paused_s !== undefined || f.paused_since !== undefined)
  const holds = writerPauses ? [] : a.holds
  const sleptMs = (s: number, e: number) => offMs(a.sleeps, s, e)
  const stopped = r.stage === 'running' && l ? stoppedAt(l) : undefined
  const offClock = (from: number, stop: number) => offMs([...holds.map(h => ({ since: h.sinceMs, until: h.untilMs ?? stop })), ...a.sleeps], from, stop)

  let stop: number | undefined
  if (r.stage === 'running' && rowStart !== undefined) {
    // A pausing writer's `started` is true as of its last write; `giverny manage`'s at the pause.
    stop = paused === undefined ? now : Math.min(written === undefined ? paused : Math.max(written, paused), now)
    if (stopped) stop = Math.min(stop, Math.max(stopped[0], rowStart))
  }
  const workSince = (from: number, upto: number) => {
    const start = Math.min(from, upto)
    return Math.floor(Math.max(0, upto - start - offClock(start, upto) - stoppedMs(l, start, upto)) / 1000)
  }
  const workS = rowStart !== undefined && stop !== undefined ? workSince(rowStart, stop) : undefined

  let elapsed = ''
  if (r.stage === 'running' && workS !== undefined) elapsed = fmtSpan(workS)
  if (r.stage === 'done') {
    const end = endedOf(r)
    if (rowStart !== undefined && end !== undefined) {
      elapsed = fmtSpan(Math.floor(Math.max(0, end - rowStart - stoppedMs(l, rowStart, end) - sleptMs(rowStart, end)) / 1000))
    }
  }

  // A worker no feed row carries: its agent ETA, counted from its start.
  const agentEta = !f && l ? a.etas[l.id] : undefined
  const totalS = agentEta && rowStart !== undefined ? Math.floor(Math.max(0, agentEta.atMs - rowStart) / 1000) + agentEta.leftS : undefined
  const etaS = f?.eta_s ?? totalS
  const figure: [number, number] | undefined = lastFigure(r) ?? (agentEta ? [rowStart === undefined ? agentEta.atMs : Math.max(agentEta.atMs, rowStart), agentEta.leftS] : undefined)

  let delta: [number, string] | undefined
  if (r.stage === 'done') {
    delta = etaDelta(r, sleptMs)
    if (!delta && agentEta) {
      const at = rowStart === undefined ? agentEta.firstAtMs : Math.max(agentEta.firstAtMs, rowStart)
      const end = endedOf(r)
      if (end !== undefined) {
        const upto = Math.max(end, at)
        delta = doneDelta(Math.floor(Math.max(0, upto - at - sleptMs(at, upto)) / 1000), undefined, agentEta.firstLeftS)
      }
    }
  }

  let leftS: number | undefined
  if (r.stage === 'running') {
    if (figure && stop !== undefined) leftS = figure[1] - workSince(figure[0], stop)
    else if (etaS !== undefined && workS !== undefined) leftS = etaS - workS
  }
  let eta = ''
  if (r.stage === 'running' && leftS !== undefined) eta = countdown(leftS)
  if (r.stage === 'planned' && etaS !== undefined) eta = `~${fmtSpan(etaS)}`
  if (r.stage === 'done' && delta) eta = fmtDelta(delta[0])
  const noEta = r.stage === 'running' && etaS === undefined
  const over = r.stage === 'running' && leftS !== undefined && leftS < 0 && fmtSpan(leftS) !== '0m'

  const lease = r.stage !== 'done' ? f?.lease : undefined
  const measured = f?.usage
  const oom = measured?.oom_open === true && r.stage !== 'planned'
  const limit = limitOut(a, now)
  const waiting: Waiting | undefined = r.stage === 'running' && f?.waiting_since !== undefined ? { since: f.waiting_since, note: f.note } : undefined
  const u = r.stage === 'running' ? unworked(r, now) : undefined
  const flag = u?.kind === 'stopped' ? `stopped: ${u.reason}` : undefined
  const asleep = r.stage === 'running' ? asleepOn(r, a, now) : undefined
  const stillFinishing = r.stage === 'running' && f?.paused_since !== undefined && l !== undefined && isRunning(l) && !killed(r)
  const benchedFinished = () => {
    const un = unworked(r, now)
    if (!un || un.kind !== 'finished') return false
    // Its worker ended its turn before the pause: work done. After it, it stopped for the pause.
    return !(f?.paused_since !== undefined && un.endedMs !== undefined && un.endedMs >= f.paused_since)
  }

  let what = ''
  if (r.stage === 'running') {
    if (killed(r)) what = KILLED
    else if (flag) what = flag
    else if (limit) what = limitNote(limit.window, limit.reopensMs, now)
    else if (paused !== undefined) what = stillFinishing ? PAUSED_FINISHING : `paused since ${clockAt(paused, now)}`
    else if (stopped) what = stopped[1]
    else if (u?.kind === 'finished') what = asleep ? asleepNow(asleep, waiting, now) : turnEnded(u.endedMs, waiting, now)
    else if (l && isRunning(l)) what = l.activity ?? ''
  } else if (r.stage === 'planned') {
    if (benchedPause !== undefined && !wait?.held) what = benchedFinished() ? PAUSED_FINISHED : `paused since ${clockAt(benchedPause, now)}`
    else what = wait?.note ?? ''
  } else {
    what = f?.landing ?? (r.nextKey ? `→ ${r.nextKey}` : '') ?? ''
    if (!what && l && !isRunning(l)) what = OUTCOME[l.status] ?? 'done'
  }

  const tokenCount = tokensOf(r)
  const tokens = tokenCount === undefined ? '' : fmtTokens(tokenCount)
  const leaseWords = lease ? leaseFact(lease) : ''

  // The header over the worker, from the cells above before any ditto.
  const some = (s: string, make: (s: string) => string) => (s ? [make(s)] : [])
  const facts =
    r.stage === 'done'
      ? [...some(what, s => s), ...some(elapsed, s => `took ${s}`), ...some(eta, s => `${s} vs ${delta?.[1] ?? 'estimate'}`), ...some(tokens, s => `${s} tokens`)]
      : r.stage === 'running'
        ? [...some(elapsed, s => `running ${s}`), ...some(eta, s => `${s} left`), ...some(what, s => s), ...some(tokens, s => `${s} tokens`)]
        : some(eta, s => `est ${s}`)
  if (noEta) facts.splice(Math.min(1, facts.length), 0, noEtaHint(key, l?.id))
  if (wait?.held) facts.push(`held: ${wait.held}`)
  if (leaseWords) facts.push(leaseWords)
  if (measured && r.stage !== 'planned') {
    const fact = usageFact(measured)
    if (fact) facts.push(fact)
  }

  // The hover's lines under the title: where the row is and why.
  let why: string[] = []
  const note = f?.note
  if (r.stage === 'running') {
    const model = shortModel(l?.model)
    const who = l ? `worker ${displayName(l)}${model ? ` (${model})` : ''}` : f?.live ? 'no worker; its own commands run' : 'no worker on it'
    const startAt = f?.spawned ?? rowStart
    const since = startAt === undefined ? undefined : clockAt(startAt, now)
    let state: string | undefined
    if (killed(r)) state = killedWhy(r, now)
    else if (u?.kind === 'stopped') state = `stopped: ${u.reason} — its worker is not working`
    else if (u?.kind === 'finished' && asleep) state = asleepWhy(asleep, u.endedMs, now)
    else if (u?.kind === 'finished') {
      state =
        u.endedMs !== undefined
          ? `its worker ${u.how} at ${clockAt(u.endedMs, now)} — not landed yet; nothing works on it until its manager wakes the worker or lands it`
          : `its worker ${u.how} — not landed yet`
    } else if (limit) state = `${limitNote(limit.window, limit.reopensMs, now)} — out of usage; its clock is held`
    else if (paused !== undefined) {
      const at = clockAt(paused, now)
      state = stillFinishing ? `paused since ${at}, its worker still finishing its turn: only its clock is paused until it stops` : pausedWhy(at, note)
    }
    const doing = l && isRunning(l) && l.activity ? [l.activity] : []
    const waited = waiting && paused === undefined ? [waitingWhy(waiting, now)] : []
    why = [...doing, ...runningWhy(who, since, etaS, leftS, state), ...waited]
  } else if (r.stage === 'planned') {
    let line = wait?.why ?? ''
    if (benchedPause !== undefined && !wait?.held) line = what === PAUSED_FINISHED ? `${PAUSED_FINISHED} — its work is done, to land or resume` : pausedWhy(clockAt(benchedPause, now), note)
    why = [...(line ? [line] : []), ...(eta ? [`estimate ${eta}`] : [])]
  } else {
    why = doneWhy(what, elapsed, delta, f?.review)
  }
  if (nested.fold) why.push(foldWhy(nested.fold))
  if (nested.parent) why.push(`spawned by ${nested.parent}`)

  return {
    id: f?.key || l?.id || '',
    section: stageSection[r.stage],
    own: stageSection[r.stage],
    key: id,
    title,
    head: nestHead(nested.fold, nested.depth, anyFold),
    depth: nested.depth,
    fold: nested.fold,
    elapsed,
    left: noEta ? NO_ETA : eta,
    over,
    noEta,
    what,
    flag: flag !== undefined,
    tokens,
    use: useCell(r, a),
    oom,
    why,
    facts,
    lease: leaseWords,
    review: f?.review,
    landedMs: r.stage === 'done' ? endedOf(r) : undefined,
    workerId: l?.id ?? agentIdOf(r),
    finished: r.stage === 'running' && l !== undefined && !isRunning(l),
    asleep: asleep !== undefined,
    transcript: l?.transcript,
    brief: f?.brief,
    open: f?.open,
    note: f?.note,
  }
}

/** Rule 6: a cell is `"` when its row is the same worker's as above and its text is that row's. */
function dittos(rows: PaneRow[], lines: DisplayRow[]): void {
  const raw = lines.map(l => ({ elapsed: l.elapsed, left: l.noEta ? '' : l.left, what: l.what, tokens: l.tokens }))
  lines.forEach((line, i) => {
    if (i === 0 || !rows[i]?.ditto) return
    const [mine, above] = [raw[i], raw[i - 1]]
    if (!mine || !above) return
    if (mine.elapsed && mine.elapsed === above.elapsed) line.elapsed = '"'
    if (mine.left && mine.left === above.left) line.left = '"'
    if (mine.what && mine.what === above.what) line.what = '"'
    if (mine.tokens && mine.tokens === above.tokens) line.tokens = '"'
  })
}

/** The use cell: its worker's commands and its runs, once each; a Done row's peak. */
function useCell(r: PaneRow, a: Around): RowUse | undefined {
  if (r.stage === 'done') {
    const peak = r.feed?.usage?.peak_mb
    return peak ? { memMb: peak, cpuLoad: 'light', memLoad: 'light' } : undefined
  }
  if (r.stage !== 'running') return undefined
  const agent = agentIdOf(r)
  const worker = agent === undefined ? undefined : a.use?.workers[agent]
  const run = r.feed === undefined ? undefined : a.use?.tasks[r.feed.key]
  const sum = worker && run && run.agent !== agent ? plusUse(worker, run.use) : (worker ?? run?.use)
  if (!sum || (sum.cpuPct === 0 && sum.memMb === 0)) return undefined
  return { cpuPct: sum.cpuPct, memMb: sum.memMb, cpuLoad: loadOf(sum.cpuPct), memLoad: loadOf(memShare(sum.memMb, a.use?.ramMb)) }
}

const NOTHING: Around = { manager: false, sessions: [], runs: [], etas: {}, shells: {}, sleeps: [], holds: [], doneRows: 'all', folded: [], dismissed: [] }

/** The feed's rows as the pane sees them: the ledger's word, runs going, what was cleared. */
function prepared(feed: Feed | undefined, a: Around): FeedRow[] {
  let rows = feed?.rows ?? []
  if (a.clearedMs !== undefined) {
    const cut = a.clearedMs
    rows = rows.filter(r => r.stage !== 'done' || (r.ended !== undefined && r.ended > cut))
  }
  if (a.ledger) rows = withLedger(rows, a.sessions, a.ledger)
  return rows.map(r => (a.runs.includes(r.key) ? { ...r, live: true } : r))
}

const present = (agents: AgentRow[], a: Around) => agents.filter(l => isRunning(l) || !a.dismissed.includes(l.id))

/** The band's rows, in the order Giverny's pane draws them. */
export function buildModel(feed: Feed | undefined, agents: AgentRow[], now: number, around: Partial<Around> = {}): Model {
  const a: Around = { ...NOTHING, ...around }
  const written = feed?.writer === WRITER ? undefined : feed?.writtenMs
  const all = bench(merge(prepared(feed, a), present(agents, a)), now)
  // What a Next up row waits for is read off every row, drawn or not.
  const waits = all.map(r => plannedWait(r, all))
  const order = startOrder(all, waits)
  const kept = all
    .map((r, i) => ({ r, wait: waits[i], o: order[i] as { stage: number; start: Start; depth: number }, i }))
    .filter(x => keeps(x.r, a.manager))
    .sort((x, y) => x.o.stage - y.o.stage || startCmp(x.o.start, y.o.start) || x.o.depth - y.o.depth || x.i - y.i)
  const placed = kept.map(x => x.r)
  const nesting = nest(placed, a.doneRows, a.folded)
  const rows = nesting.map(n => placed[n.at] as PaneRow)
  reditto(rows)
  const anyFold = nesting.some(n => n.fold !== undefined)
  const lines = nesting.map((n, i) => formatRow(rows[i] as PaneRow, now, written, a, kept[n.at]?.wait, n, anyFold))
  dittos(rows, lines)
  // An agent under another's row goes where that row goes, whatever its own stage.
  lines.forEach((line, i) => {
    if ((nesting[i] as Nested).depth > 0) line.section = (lines[i - 1] as DisplayRow).section
  })
  const count = (s: Section) => lines.filter(l => l.section === s).length
  return { rows: lines, running: count('running'), next: count('next'), done: count('done'), footer: feed?.footer, nested: anyFold }
}

/**
 * The finished workers a clear-done takes off the band. One still behind a Running row stays:
 * that row is not done, and without it would read as having no worker.
 */
export function clearable(rows: FeedRow[], agents: AgentRow[]): string[] {
  const holding = new Set(join(rows, agents).flatMap(j => (j.row?.stage === 'running' && j.agent ? [j.agent.id] : [])))
  return agents.filter(a => !isRunning(a) && !holding.has(a.id)).map(a => a.id)
}

/** One row its manager has to act on: `<feed dir>/chores/<session>.json`. */
export type Chore = { key: string; kind: 'land' | 'landpaused' | 'nowpaused' | 'start' | 'worker'; since: number; how?: string }

const THRESHOLD: Record<Chore['kind'], number> = { land: LAND_GRACE_MS, landpaused: LAND_GRACE_MS, nowpaused: 0, start: IDLE_PLANNED_MS, worker: HANDOFF_WINDOW_MS }

function told(c: Chore, f: FeedRow | undefined): boolean {
  if (!f) return false
  if (c.kind === 'start') return f.start_nudged === c.since
  if (c.kind === 'worker') return f.worker_nudged === c.since
  if (c.kind === 'nowpaused') return f.land_nudged !== undefined && f.paused_since !== undefined && f.land_nudged >= f.paused_since
  return f.land_nudged === c.since
}

/**
 * Rule 9: what the rows ask of their manager rather than of the person, due now and not yet
 * told, and the paused rows whose workers are still finishing. Giverny's app works this out
 * for a tab of its own; for a session outside one nobody else would.
 */
export function dueChores(feed: Feed | undefined, agents: AgentRow[], now: number, around: Partial<Around> = {}): { chores: Chore[]; finishing: string[] } {
  const a: Around = { ...NOTHING, ...around }
  const all = bench(merge(prepared(feed, a), present(agents, a)), now)
  const out: Chore[] = []
  const finishing: string[] = []
  for (const r of all) {
    const f = r.feed
    if (!f || !keeps(r, a.manager)) continue
    if (r.stage === 'running' && f.paused_since !== undefined && r.live && isRunning(r.live) && !killed(r)) finishing.push(f.key)
    const u = unworked(r, now)
    if (r.stage === 'running') {
      if (killed(r) || asleepOn(r, a, now)) continue
      if (u?.kind === 'finished' && u.endedMs !== undefined) out.push({ key: f.key, kind: 'land', since: u.endedMs, how: u.how })
    } else if (r.stage === 'planned' && f.stage === 'running') {
      const paused = f.paused_since !== undefined
      if (u?.kind === 'no-worker' && !paused && f.held === undefined) out.push({ key: f.key, kind: 'worker', since: u.since })
      if (u?.kind === 'finished' && paused && u.endedMs !== undefined) {
        const stoppedForPause = f.paused_since !== undefined && u.endedMs >= f.paused_since
        out.push({ key: f.key, kind: stoppedForPause ? 'nowpaused' : 'landpaused', since: u.endedMs, how: u.how })
      }
    } else if (r.stage === 'planned') {
      const since = plannedWait(r, all)?.idleSince
      if (since !== undefined) out.push({ key: f.key, kind: 'start', since })
    }
  }
  const byKey = (k: string) => (feed?.rows ?? []).find(f => f.key === k)
  return { chores: out.filter(c => now >= c.since + THRESHOLD[c.kind] && !told(c, byKey(c.key))), finishing }
}
