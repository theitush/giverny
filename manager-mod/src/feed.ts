import type { EngineInterface } from 'claude-code'

import type { AgentRow, Feed, FeedLease, FeedRow, FeedUsage, Stage, Use } from '../types'

// This session's feed, read only when its file changed: one `stat` a poll otherwise.
// The format is docs/management-panel.md; parse() is crates/claude/src/feed.rs's forgiving
// reader: a wrong field reads as absent, a row with no stage or key is dropped.

const FEED_VERSION = 1
const SEARCH_EVERY_MS = 60_000

const STAGES: Record<string, Stage> = {
  running: 'running',
  live: 'running',
  working: 'running',
  'in progress': 'running',
  in_progress: 'running',
  planned: 'planned',
  queued: 'planned',
  next: 'planned',
  done: 'done',
  finished: 'done',
  completed: 'done',
  landed: 'done',
}
const TIMES = ['started', 'ended', 'spawned', 'paused_since', 'killed', 'went_off', 'left_at', 'eta_at', 'waiting_since', 'planned', 'unheld', 'land_nudged', 'start_nudged', 'worker_nudged'] as const
const COUNTS = ['paused_s', 'eta_s', 'eta_first_s', 'left_s', 'wait_s', 'reest_s', 'reest_at_s', 'eta_guess_s', 'tokens', 'task_tokens'] as const
const TEXTS = ['title', 'agent_id', 'repo', 'landing', 'group', 'brief', 'open', 'note', 'review', 'after', 'held'] as const

const text = (v: unknown): string | undefined => {
  if (typeof v === 'number') return String(v)
  if (typeof v !== 'string') return undefined
  return v.trim() || undefined
}

const whole = (v: unknown): number | undefined => {
  const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : v
  return typeof n === 'number' && Number.isFinite(n) ? Math.trunc(n) : undefined
}

const count = (v: unknown): number | undefined => {
  const n = whole(v)
  return n !== undefined && n >= 0 ? n : undefined
}

const time = (v: unknown): number | undefined => {
  if (typeof v === 'number') return count(v)
  if (typeof v !== 'string') return undefined
  const n = Date.parse(v.trim())
  return Number.isNaN(n) ? undefined : n
}

const texts = (v: unknown) => (Array.isArray(v) ? v.flatMap(s => text(s) ?? []) : [])

function parseLease(v: unknown): FeedLease | undefined {
  if (typeof v !== 'object' || v === null) return undefined
  const l = v as Record<string, unknown>
  const state = typeof l.state === 'string' ? l.state.trim().toLowerCase() : ''
  if (state !== 'granted' && state !== 'smaller' && state !== 'queued') return undefined
  return {
    state,
    id: text(l.id),
    ram_mb: count(l.ram_mb) ?? 0,
    gpus: Array.isArray(l.gpus) ? l.gpus.flatMap(g => count(g) ?? []) : [],
    vram_mb: count(l.vram_mb) ?? 0,
    slots: texts(l.slots),
    granted_ms: time(l.granted_at),
    wanted_ram_mb: count(l.wanted_ram_mb),
    position: count(l.position),
    behind: text(l.behind),
    short: texts(l.short),
    uncollected: l.uncollected === true,
    lapsed: l.lapsed === true,
  }
}

const OOM_EXIT = 137

function parseUsage(v: unknown): FeedUsage | undefined {
  if (typeof v !== 'object' || v === null) return undefined
  const u = v as Record<string, unknown>
  const open = typeof u.oom_open === 'boolean' ? u.oom_open : (count(u.oom_kills) ?? 0) > 0 && whole(u.last_exit) === OOM_EXIT
  return {
    ...(u as FeedUsage),
    runs: count(u.runs),
    peak_mb: count(u.peak_mb),
    cpu_s: count(u.cpu_s),
    wall_s: count(u.wall_s),
    oom_kills: count(u.oom_kills),
    cap_ram_mb: count(u.cap_ram_mb),
    oom_open: open,
  }
}

function parseDevAgent(v: unknown): AgentRow | undefined {
  if (typeof v !== 'object' || v === null) return undefined
  const a = v as Record<string, unknown>
  const id = text(a.id)
  if (!id) return undefined
  return {
    id,
    description: text(a.description) ?? '',
    status: text(a.status) ?? 'running',
    parentId: text(a.parentId),
    name: text(a.name),
    startedMs: time(a.started),
    endedMs: time(a.ended),
    lastTurnMs: time(a.last_turn),
    tokens: count(a.tokens),
    activity: text(a.activity),
    transcript: text(a.transcript),
    stops: Array.isArray(a.stops)
      ? a.stops.flatMap(s => {
          const fromMs = time((s as { from?: unknown }).from)
          return fromMs === undefined ? [] : [{ fromMs, toMs: time((s as { to?: unknown }).to), reason: text((s as { reason?: unknown }).reason) ?? 'API error' }]
        })
      : undefined,
    shells: Array.isArray(a.shells)
      ? a.shells.map(s => {
          const raw = s as { id?: unknown; cmd?: unknown; started?: unknown }
          return { id: text(raw.id) ?? '', cmd: text(raw.cmd) ?? '', startedMs: time(raw.started) ?? 0 }
        })
      : undefined,
  }
}

function parseRow(v: unknown): FeedRow | undefined {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return undefined
  const raw = v as Record<string, unknown>
  const stage = typeof raw.stage === 'string' ? STAGES[raw.stage.trim().toLowerCase()] : undefined
  const key = text(raw.key) ?? text(raw.agent_id)
  if (!stage || !key) return undefined
  const row: Record<string, unknown> = { key, stage }
  for (const f of TIMES) row[f] = time(raw[f])
  for (const f of COUNTS) row[f] = count(raw[f])
  for (const f of TEXTS) row[f] = text(raw[f])
  row.eta_delta_s = whole(raw.eta_delta_s)
  row.follows_worker = raw.follows_worker === true || undefined
  row.lease = parseLease(raw.lease)
  row.usage = parseUsage(raw.usage)
  return row as FeedRow
}

/** A feed from its text; undefined when the whole file is refused (not JSON, too new). */
export function parse(source: string): Feed | undefined {
  let v: unknown
  try {
    v = JSON.parse(source)
  } catch {
    return undefined
  }
  if (typeof v !== 'object' || v === null || Array.isArray(v)) return undefined
  const raw = v as Record<string, unknown>
  if ((count(raw.version) ?? 1) > FEED_VERSION) return undefined
  const footer = raw.footer
  return {
    session: text(raw.session),
    name: text(raw.name),
    writer: text(raw.writer),
    aliases: Array.isArray(raw.aliases) ? raw.aliases.flatMap(a => text(a) ?? []) : [],
    footer: typeof footer === 'object' && footer !== null ? text((footer as { text?: unknown }).text) : text(footer),
    rows: Array.isArray(raw.rows) ? raw.rows.flatMap(r => parseRow(r) ?? []) : [],
    dev_agents: Array.isArray(raw.dev_agents) ? raw.dev_agents.flatMap(a => parseDevAgent(a) ?? []) : undefined,
    dev_use: typeof raw.dev_use === 'object' && raw.dev_use !== null ? (raw.dev_use as Record<string, Use>) : undefined,
  }
}

export const namesSession = (f: Feed, session: string) => f.session === session || f.aliases.includes(session)

export async function feedDir($: EngineInterface): Promise<string> {
  const set = await $.env.get('GIVERNY_FEED_DIR')
  if (set) return set
  const config = (await $.env.get('XDG_CONFIG_HOME')) || `${await $.env.get('HOME')}/.config`
  return `${config}/giverny/feeds`
}

const stamp = (f: { mtimeMs: number; size: number }) => `${f.mtimeMs}:${f.size}`

/** This session's last good feed and where it came from; keep one for pollFeed(). */
export type FeedWatch = {
  session: string
  path: string
  stamp: string
  feed: Feed | undefined
  searchedAt: number
  /** Files that named another session, by their stamp, so a search reads only what moved. */
  notOurs: Map<string, string>
}

export const newWatch = (): FeedWatch => ({ session: '', path: '', stamp: '', feed: undefined, searchedAt: 0, notOurs: new Map() })

/** This session's feed now, as of its last good read. Costs a `stat` when nothing moved. */
export async function pollFeed($: EngineInterface, w: FeedWatch, now: number): Promise<Feed | undefined> {
  const session = await $.session.id()
  const dir = await feedDir($)
  if (session !== w.session) Object.assign(w, newWatch(), { session })
  // The session's own file first; a writer that kept its name after a re-id is searched for.
  if (await follow($, w, `${dir}/${session}.json`)) return w.feed
  if (w.path && (await follow($, w, w.path))) return w.feed
  w.path = ''
  w.stamp = ''
  w.feed = undefined
  if (now - w.searchedAt >= SEARCH_EVERY_MS) {
    w.searchedAt = now
    await search($, w, dir, session)
  }
  return w.feed
}

/** Reads `path` if it changed. False when it is not there. */
async function follow($: EngineInterface, w: FeedWatch, path: string): Promise<boolean> {
  let found
  try {
    found = await $.fs.stat(path)
  } catch {
    return false
  }
  if (found.kind !== 'file') return false
  if (path === w.path && stamp(found) === w.stamp) return true
  w.path = path
  w.stamp = stamp(found)
  try {
    // Torn or refused: the last good read stays on screen.
    const read = parse(await $.fs.read(path))
    if (read) w.feed = { ...read, writtenMs: found.mtimeMs }
  } catch {
    // Gone or being replaced: the next poll sees it.
  }
  return true
}

async function search($: EngineInterface, w: FeedWatch, dir: string, session: string): Promise<void> {
  let entries
  try {
    entries = await $.fs.list(dir)
  } catch {
    return
  }
  const feeds = entries
    .filter(f => f.kind === 'file' && f.name.endsWith('.json') && w.notOurs.get(f.name) !== stamp(f))
    .sort((a, b) => b.mtimeMs - a.mtimeMs)
  for (const f of feeds) {
    let feed
    try {
      feed = parse(await $.fs.read(`${dir}/${f.name}`))
    } catch {
      continue
    }
    if (feed && namesSession(feed, session)) {
      w.path = `${dir}/${f.name}`
      w.stamp = stamp(f)
      w.feed = { ...feed, writtenMs: f.mtimeMs }
      return
    }
    w.notOurs.set(f.name, stamp(f))
  }
}
