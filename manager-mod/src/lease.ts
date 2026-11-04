import type { FeedLease, FeedRow, FeedUsage, Ledger, LedgerLease, LedgerWaiter } from '../types'

import { fmtSpan } from './time'

// What a row holds or waits for in the machine ledger, and what its commands used, in the
// panel's words (management_panel.rs queued_note, lease_fact, usage_fact, with_ledger;
// resources.rs short_need, describe_held, in_the_way, ordered_queue). Plain data, no `$`.

const STALE_MS = 20 * 60 * 1000

export const GRANTED_WAITING = 'granted, waiting for its manager'

/** A size as it was set: `3G`, `1.5G`, `512M`. */
export function mem(mb: number): string {
  if (mb === 0) return '0'
  if (mb >= 1024) return mb % 1024 === 0 ? `${mb / 1024}G` : `${(mb / 1024).toFixed(1)}G`
  return `${mb}M`
}

/** A measured size in G: `0.5G`, `4.2G`, `12G`, `1.5T`. */
export function gb(mb: number): string {
  if (mb === 0) return '0.0G'
  let v = mb / 1024
  let unit = 'G'
  if (v >= 999.5) {
    v /= 1024
    unit = 'T'
  }
  return v < 9.95 ? `${Math.max(v, 0.1).toFixed(1)}${unit}` : `${Math.round(v)}${unit}`
}

/** What a queued request is short of, in its own sizes: `CPU`, `3G`, `CPU, 3G`, `gpu 8G`, `a slot`. */
export function shortNeed(short: string[], ramMb: number, vramMb: number): string {
  const parts = short.map(s => {
    if (s === 'cpu') return 'CPU'
    if (s === 'ram') return ramMb > 0 ? mem(ramMb) : 'RAM'
    if (s === 'gpu') return vramMb > 0 ? `gpu ${mem(vramMb)}` : 'a GPU'
    return 'a slot'
  })
  return [...new Set(parts)].join(', ')
}

/** A Next up row waiting in the ledger: `queued for 3G behind 899x`. */
export function queuedNote(l: FeedLease): string {
  if (l.lapsed) return 'queued, grant lapsed, waiting for its manager'
  const need = shortNeed(l.short, l.ram_mb, l.vram_mb)
  return `queued${need ? ` for ${need}` : ''}${l.behind ? ` behind ${l.behind}` : ''}`
}

/** The queued note in a row's hover. */
export function queuedWhy(l: FeedLease): string {
  if (l.lapsed) return 'queued — its grant lapsed untaken; waiting for its manager'
  const need = shortNeed(l.short, l.ram_mb, l.vram_mb)
  return `queued — waiting${need ? ` for ${need}` : ''}${l.behind ? ` behind ${l.behind}` : ''} in the machine ledger`
}

function describeHeld(l: FeedLease): string {
  const parts = [...(l.ram_mb > 0 ? [mem(l.ram_mb)] : []), ...l.gpus.map(g => `gpu${g} ${mem(l.vram_mb)}`), ...l.slots.map(s => `slot ${s}`)]
  return parts.length > 0 ? parts.join(', ') : 'nothing'
}

/** The lease in full: `holds 3G, slot cargo:/x`, or `queued #2 for 3G behind y`. */
export function leaseFact(l: FeedLease): string {
  if (l.state === 'queued') {
    const note = queuedNote(l)
    return l.position === undefined ? note : note.replace('queued', `queued #${l.position}`)
  }
  const asked = l.state === 'smaller' && l.wanted_ram_mb !== undefined ? ` (asked ${mem(l.wanted_ram_mb)})` : ''
  return `holds ${describeHeld(l)}${asked}${l.uncollected ? `; ${GRANTED_WAITING}` : ''}`
}

/** What its commands used: `peak 2.1G of 3G, 45s CPU, OOM-killed x1 earlier`. */
export function usageFact(u: FeedUsage): string {
  const parts: string[] = []
  if (u.peak_mb !== undefined && u.peak_mb > 0) {
    parts.push(`peak ${gb(u.peak_mb)}${u.cap_ram_mb !== undefined && u.cap_ram_mb > 0 ? ` of ${mem(u.cap_ram_mb)}` : ''}`)
  }
  const cpu = u.cpu_s ?? 0
  if (cpu > 0) parts.push(cpu < 60 ? `${cpu}s CPU` : `${fmtSpan(cpu)} CPU`)
  if ((u.oom_kills ?? 0) > 0) parts.push(`OOM-killed x${u.oom_kills}${u.oom_open ? '' : ' earlier'}`)
  return parts.join(', ')
}

const time = (v: unknown): number | undefined => {
  if (typeof v === 'number') return v
  if (typeof v !== 'string') return undefined
  const n = Date.parse(v)
  return Number.isNaN(n) ? undefined : n
}
const num = (v: unknown) => (typeof v === 'number' && v >= 0 ? v : 0)
const strings = (v: unknown) => (Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string') : [])

/** The ledger file, less what has expired; undefined when it cannot be read or is too new. */
export function parseLedger(source: string, now: number): Ledger | undefined {
  let v: unknown
  try {
    v = JSON.parse(source)
  } catch {
    return undefined
  }
  if (typeof v !== 'object' || v === null) return undefined
  const raw = v as { version?: unknown; leases?: unknown; queue?: unknown }
  if (typeof raw.version === 'number' && raw.version > 1) return undefined
  const alive = (e: Record<string, unknown>) => {
    const beat = time(e.heartbeat_at)
    return beat !== undefined && now - beat < STALE_MS
  }
  const entries = (list: unknown) =>
    (Array.isArray(list) ? list : []).filter((e): e is Record<string, unknown> => typeof e === 'object' && e !== null && typeof e.task === 'string').filter(alive)
  const leases = entries(raw.leases).map(
    (e): LedgerLease => ({
      id: String(e.id ?? ''),
      session: String(e.session ?? ''),
      task: String(e.task),
      ram_mb: num(e.ram_mb),
      gpus: Array.isArray(e.gpus) ? e.gpus.filter((g): g is number => typeof g === 'number') : [],
      vram_mb: num(e.vram_mb),
      slots: strings(e.slots),
      granted_ms: time(e.granted_at),
      uncollected: e.uncollected !== undefined && e.uncollected !== null,
    }),
  )
  const queue = entries(raw.queue).map(
    (e): LedgerWaiter => ({
      session: String(e.session ?? ''),
      task: String(e.task),
      ram_mb: num(e.ram_mb),
      gpu: num(e.gpu),
      vram_mb: num(e.vram_mb),
      slots: strings(e.slots),
      priority: typeof e.priority === 'string' ? e.priority : undefined,
      queued_ms: time(e.queued_at) ?? 0,
      lapsed: e.lapsed !== undefined && e.lapsed !== null,
    }),
  )
  return { leases, queue }
}

const RANK: Record<string, number> = { asap: 0, urgent: 0, p0: 0, high: 1, p1: 1, medium: 2, normal: 2, p2: 2, low: 3, p3: 3 }
const rank = (p: string | undefined) => RANK[p?.trim().toLowerCase() ?? ''] ?? 4

/** First come, first served, a Priority ahead of none. */
export const orderedQueue = (l: Ledger) => [...l.queue].sort((a, b) => rank(a.priority) - rank(b.priority) || a.queued_ms - b.queued_ms)

/** Who is really in the way of a request short of `short`: the request ahead that wants it, else a lease holding it. */
export function inTheWay(leases: LedgerLease[], ahead: LedgerWaiter[], req: LedgerWaiter, short: string[]): string | undefined {
  const slotOf = (s: string) => (s.startsWith('slot ') ? s.slice(5) : undefined)
  const wants = (w: LedgerWaiter) =>
    short.some(s => {
      if (s === 'cpu' || s === 'ram') return true
      if (s === 'gpu') return w.gpu > 0
      const slot = slotOf(s)
      return slot !== undefined && w.slots.includes(slot)
    })
  const blocks = (l: LedgerLease) =>
    short.some(s => {
      if (s === 'ram') return true
      if (s === 'cpu') return false // Which leases are building the plugin does not know, as the pane does not.
      if (s === 'gpu') return l.gpus.length > 0
      const slot = slotOf(s)
      return slot !== undefined && l.slots.includes(slot) && req.slots.includes(slot)
    })
  return ahead.find(wants)?.task ?? leases.find(blocks)?.task
}

/**
 * The rows with what the ledger says now: a lease that expired or was released, or a holder
 * that left the queue, is gone though the row's copy still names it. A queued row is short
 * of what its copy says (the ledger keeps no verdict), behind whoever is in the way now.
 */
export function withLedger(rows: FeedRow[], sessions: string[], ledger: Ledger): FeedRow[] {
  const mine = (s: string) => sessions.includes(s)
  const queue = orderedQueue(ledger)
  return rows.map(row => {
    const copy = row.lease
    const held = ledger.leases.find(l => l.task === row.key && mine(l.session))
    if (held) {
      const smaller = copy?.state === 'smaller' && copy.id === held.id
      return {
        ...row,
        lease: {
          state: smaller ? 'smaller' : 'granted',
          id: held.id,
          ram_mb: held.ram_mb,
          gpus: held.gpus,
          vram_mb: held.vram_mb,
          slots: held.slots,
          granted_ms: held.granted_ms,
          wanted_ram_mb: smaller ? copy?.wanted_ram_mb : undefined,
          short: [],
          uncollected: held.uncollected,
          lapsed: false,
        },
      }
    }
    const at = queue.findIndex(w => w.task === row.key && mine(w.session))
    const waiter = queue[at]
    if (!waiter) return { ...row, lease: undefined }
    const ahead = queue.slice(0, at)
    const short = copy?.state === 'queued' ? copy.short : []
    const still = (t: string) => ledger.leases.some(l => l.task === t) || ahead.some(a => a.task === t)
    const behind =
      short.length > 0
        ? inTheWay(ledger.leases, ahead, waiter, short)
        : ((copy?.behind !== undefined && still(copy.behind) ? copy.behind : undefined) ??
          ledger.leases.find(l => l.slots.some(s => waiter.slots.includes(s)))?.task ??
          ahead.at(-1)?.task)
    return {
      ...row,
      lease: {
        state: 'queued',
        ram_mb: waiter.ram_mb,
        gpus: [],
        vram_mb: waiter.vram_mb,
        slots: waiter.slots,
        position: at + 1,
        behind,
        short,
        uncollected: false,
        lapsed: waiter.lapsed,
      },
    }
  })
}
