import type { EngineInterface } from 'claude-code'

import type { AgentEta, AgentRow, Around, DoneRows, Feed, Hold, Ledger, Shell, Span } from '../types'

import { feedDir } from './feed'
import { givernyHookOn } from './giverny'
import { parseLedger } from './lease'
import { isRunning } from './model'
import { spent, track } from './time'
import { firstStamp, foldTranscript, mergeStops } from './transcript'

// What a row's words read beyond the feed and `$.agent.list()`. Giverny's app has most of this
// from Claude Code's subagentStatusLine and its own threads; here each is a file beside the
// feeds or a transcript, read only when its stat moved, and every read is forgiving: a file
// that cannot be read costs that fact and never the band.

const TAIL_BYTES = 256 * 1024
const AGENT_READ_MS = 6_000
const RUNS_READ_MS = 6_000
const MARK_READ_MS = 5_000
const USAGE_READ_MS = 30_000
const SHELLS_READ_MS = 4_000
const SLOW_READ_MS = 30_000
const SHELL_MAX_AGE_MS = 2 * 24 * 3600 * 1000

type Cached = { stamp: string; text?: string; checkedAt: number }
type AgentFacts = {
  stamp: string
  readAt: number
  startedMs?: number
  firstRead: boolean
  lastMs?: number
  lastTurnMs?: number
  tokens?: number
  activity?: string
  model?: string
  stops: AgentRow['stops']
  transcript?: string
}
/** A background shell: the processes holding its output file when first looked at, or ended. */
type ShellState = { pids: [number, string][] } | 'ended'

export type LiveWatch = {
  files: Map<string, Cached>
  agents: Map<string, AgentFacts>
  /** Every agent seen: `$.agent.list()` forgets a finished one, the pane keeps it until cleared. */
  seen: Map<string, AgentRow>
  shells: Map<string, ShellState>
  holds: Hold[]
  manager: boolean
  markReadAt: number
  runs: string[]
  runsReadAt: number
  usageReadAt: number
  limit?: Around['limit']
  subagentDirs: Map<string, string>
  session: string
}

export const newLive = (): LiveWatch => ({
  files: new Map(),
  agents: new Map(),
  seen: new Map(),
  shells: new Map(),
  holds: [],
  manager: false,
  markReadAt: 0,
  runs: [],
  runsReadAt: 0,
  usageReadAt: 0,
  subagentDirs: new Map(),
  session: '',
})

const safeWord = (s: string) => /^[\w-]+$/.test(s)

/** `path`'s text, looked at every `everyMs` and read only when its stat moved; undefined when it is not there. */
async function textOf($: EngineInterface, w: LiveWatch, path: string, everyMs: number, now: number): Promise<string | undefined> {
  const known = w.files.get(path)
  if (known && now - known.checkedAt < everyMs) return known.text
  let stat
  try {
    stat = await $.fs.stat(path)
  } catch {
    w.files.set(path, { stamp: '', checkedAt: now })
    return undefined
  }
  const stamp = `${stat.mtimeMs}:${stat.size}`
  if (known?.stamp === stamp) {
    known.checkedAt = now
    return known.text
  }
  try {
    const text = await $.fs.read(path)
    w.files.set(path, { stamp, text, checkedAt: now })
    return text
  } catch {
    return known?.text
  }
}

async function configDir($: EngineInterface): Promise<string> {
  const set = await $.env.get('CLAUDE_CONFIG_DIR')
  return set || `${await $.env.get('HOME')}/.claude`
}

/** Where this session's agents keep their transcripts. */
async function subagentsDir($: EngineInterface, w: LiveWatch, session: string): Promise<string> {
  const known = w.subagentDirs.get(session)
  if (known) return known
  let root = ''
  try {
    root = await $.session.cwd()
  } catch {
    return ''
  }
  const slug = root.replace(/[^A-Za-z0-9]/g, '-')
  const dir = `${await configDir($)}/projects/${slug}/${session}/subagents`
  w.subagentDirs.set(session, dir)
  return dir
}

/** One agent's facts from its transcript, re-read at most every AGENT_READ_MS while it moves. */
async function factsOf($: EngineInterface, w: LiveWatch, dirs: string[], id: string, now: number): Promise<AgentFacts | undefined> {
  const known = w.agents.get(id)
  if (known && now - known.readAt < AGENT_READ_MS) return known
  for (const dir of dirs) {
    const path = `${dir}/agent-${id}.jsonl`
    let stat
    try {
      stat = await $.fs.stat(path)
    } catch {
      continue
    }
    const stamp = `${stat.mtimeMs}:${stat.size}`
    if (known?.stamp === stamp) {
      known.readAt = now
      return known
    }
    const facts: AgentFacts = known ?? { stamp, readAt: now, firstRead: false, stops: [], transcript: path }
    facts.stamp = stamp
    facts.readAt = now
    try {
      if (!facts.firstRead) {
        const head = await $.process.run(['head', '-n', '1', path])
        facts.startedMs = firstStamp(head.stdout)
        facts.firstRead = true
      }
      const tail = await $.process.run(['tail', '-c', String(TAIL_BYTES), path])
      const found = foldTranscript(tail.stdout)
      facts.lastMs = found.lastMs ?? facts.lastMs
      facts.lastTurnMs = found.lastTurnMs ?? facts.lastTurnMs
      facts.tokens = found.tokens ?? facts.tokens
      facts.activity = found.activity ?? facts.activity
      facts.model = found.model ?? facts.model
      facts.stops = mergeStops(facts.stops ?? [], found.stops)
    } catch {
      // Keep what the last read found.
    }
    w.agents.set(id, facts)
    return facts
  }
  return known
}

/** `$.agent.list()` with its transcript's facts, and the finished ones it has since forgotten. */
export async function readAgents($: EngineInterface, w: LiveWatch, sessions: string[], now: number): Promise<AgentRow[]> {
  // A `/clear` or a `/resume` into another conversation shows that one's workers instead; a
  // re-id of the same conversation names the old id as an alias and keeps them.
  if (w.session !== '' && !sessions.includes(w.session)) Object.assign(w, newLive())
  w.session = sessions[0] ?? ''
  const listed = await $.agent.list()
  const dirs: string[] = []
  for (const s of sessions) dirs.push(await subagentsDir($, w, s))
  for (const a of listed) {
    const facts = await factsOf($, w, dirs, a.id, now)
    const running = isRunning({ id: a.id, description: a.description, status: a.status })
    w.seen.set(a.id, {
      id: a.id,
      description: a.description,
      status: a.status,
      parentId: a.parentId,
      name: a.name,
      startedMs: facts?.startedMs,
      endedMs: running ? undefined : (facts?.lastMs ?? w.seen.get(a.id)?.endedMs ?? now),
      lastTurnMs: facts?.lastTurnMs,
      tokens: facts?.tokens,
      activity: running ? facts?.activity : undefined,
      model: facts?.model,
      stops: facts?.stops,
      transcript: facts?.transcript,
    })
  }
  // One that left the list while running ended about then.
  const ids = new Set(listed.map(a => a.id))
  for (const [id, a] of w.seen) {
    if (!ids.has(id) && isRunning(a)) w.seen.set(id, { ...a, status: 'completed', activity: undefined, endedMs: now })
  }
  return [...w.seen.values()]
}

export function parseEtas(text: string): Record<string, AgentEta> {
  const out: Record<string, AgentEta> = {}
  try {
    const raw = JSON.parse(text) as { agents?: Record<string, { left_s?: unknown; at?: unknown; first_left_s?: unknown; first_at?: unknown }> }
    for (const [id, e] of Object.entries(raw.agents ?? {})) {
      const atMs = typeof e.at === 'string' ? Date.parse(e.at) : Number(e.at)
      const leftS = Number(e.left_s)
      if (!Number.isFinite(atMs) || !Number.isFinite(leftS)) continue
      const firstAt = typeof e.first_at === 'string' ? Date.parse(e.first_at) : Number(e.first_at)
      const firstLeft = Number(e.first_left_s)
      out[id] = { leftS, atMs, firstLeftS: Number.isFinite(firstLeft) ? firstLeft : leftS, firstAtMs: Number.isFinite(firstAt) ? firstAt : atMs }
    }
  } catch {
    // A torn write: nothing known yet.
  }
  return out
}

export function parseSleeps(text: string): Span[] {
  return text.split('\n').flatMap(line => {
    try {
      const v = JSON.parse(line) as { since?: unknown; until?: unknown }
      return typeof v.since === 'number' && typeof v.until === 'number' ? [{ since: v.since, until: v.until }] : []
    } catch {
      return []
    }
  })
}

/** `management_panel.done_rows` and `done_last`; an unknown word shows all. */
export function parseDoneRows(toml: string): DoneRows {
  const section = /^\[management_panel\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(toml)?.[1] ?? ''
  const word = /^\s*done_rows\s*=\s*"([^"]*)"/m.exec(section)?.[1]?.trim().toLowerCase()
  if (word === 'hide' || word === 'none') return 'hide'
  if (word !== 'last') return 'all'
  const last = /^\s*done_last\s*=\s*(\d+)/m.exec(section)?.[1]
  return last === undefined ? 5 : Number(last)
}

type RecordedShell = { agent?: unknown; cmd?: unknown; id?: unknown; started_ms?: unknown }

export function parseShells(text: string): { agent: string; shell: Shell }[] {
  try {
    const raw = JSON.parse(text) as { shells?: RecordedShell[] }
    return (raw.shells ?? []).flatMap(s =>
      typeof s.agent === 'string' && typeof s.id === 'string' && typeof s.cmd === 'string' && typeof s.started_ms === 'number'
        ? [{ agent: s.agent, shell: { id: s.id, cmd: s.cmd, startedMs: s.started_ms } }]
        : [],
    )
  } catch {
    return []
  }
}

/** A process's start, to tell it from another that came back with its pid. */
async function startOf($: EngineInterface, pid: number): Promise<string | undefined> {
  try {
    const stat = await $.fs.read(`/proc/${pid}/stat`)
    return stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19]
  } catch {
    return undefined
  }
}

/**
 * Does this shell still run? Its output file's holders are looked for in /proc once; after
 * that only whether one of them runs. Ended is final, so a row never flickers.
 */
async function shellRuns($: EngineInterface, w: LiveWatch, session: string, shell: Shell, now: number): Promise<boolean> {
  let state = w.shells.get(shell.id)
  if (state === 'ended') return false
  if (now - shell.startedMs > SHELL_MAX_AGE_MS) {
    w.shells.set(shell.id, 'ended')
    return false
  }
  if (!state) {
    if (!safeWord(session) || !safeWord(shell.id)) return false
    let holders
    try {
      const found = await $.process.run(['sh', '-c', `find /proc/[0-9]*/fd -lname '*/${session}/tasks/${shell.id}.output' 2>/dev/null`])
      holders = found.stdout
    } catch {
      return false // A look that cannot read /proc decides nothing.
    }
    const pids = [...new Set(holders.split('\n').flatMap(l => /^\/proc\/(\d+)\/fd\//.exec(l)?.[1] ?? []))].map(Number)
    const known: [number, string][] = []
    for (const pid of pids) {
      const start = await startOf($, pid)
      if (start) known.push([pid, start])
    }
    state = { pids: known }
    w.shells.set(shell.id, state)
  }
  for (const [pid, start] of state.pids) {
    if ((await startOf($, pid)) === start) return true
  }
  w.shells.set(shell.id, 'ended')
  return false
}

/** The shells each finished worker started that still run. */
async function readShells($: EngineInterface, w: LiveWatch, dir: string, session: string, agents: AgentRow[], now: number): Promise<Record<string, Shell[]>> {
  const out: Record<string, Shell[]> = {}
  const text = await textOf($, w, `${dir}/bgshells/${session}.json`, SHELLS_READ_MS, now)
  if (text === undefined) return out
  const finished = new Set(agents.filter(a => !isRunning(a)).map(a => a.id))
  for (const { agent, shell } of parseShells(text)) {
    if (!finished.has(agent)) continue
    if (await shellRuns($, w, session, shell, now)) (out[agent] ??= []).push(shell)
  }
  return out
}

/** The keys with a `giverny-manage run` of this session going. */
async function readRuns($: EngineInterface, w: LiveWatch, ledgerDir: string, sessions: string[], now: number): Promise<string[]> {
  if (now - w.runsReadAt < RUNS_READ_MS) return w.runs
  w.runsReadAt = now
  const keys: string[] = []
  try {
    for (const f of await $.fs.list(`${ledgerDir}/runs`)) {
      if (!f.name.endsWith('.live')) continue
      const raw = JSON.parse(await $.fs.read(`${ledgerDir}/runs/${f.name}`)) as { task?: unknown; session?: unknown; pid?: unknown }
      if (typeof raw.task !== 'string' || typeof raw.session !== 'string' || !sessions.includes(raw.session)) continue
      // One left by a killed `run` is skipped.
      if (await $.fs.exists(`/proc/${String(raw.pid)}`)) keys.push(raw.task)
    }
  } catch {
    return w.runs
  }
  w.runs = keys
  return keys
}

/**
 * Giverny's hook marks a session when the manage skill runs, and with that hook on nothing else
 * does (a stray row of a session's own is no manager, giverny#357). With no such hook nothing
 * could ever mark it, so there a Next up row does: planning is what a manager does, and a plain
 * session that only spawns workers has none. Once a manager, always one for this conversation.
 */
async function readManager($: EngineInterface, w: LiveWatch, dir: string, feed: Feed | undefined, sessions: string[], now: number): Promise<boolean> {
  if (w.manager || now - w.markReadAt < MARK_READ_MS) return w.manager
  w.markReadAt = now
  for (const s of sessions) {
    try {
      await $.fs.stat(`${dir}/managers/${s}.json`)
      w.manager = true
      return true
    } catch {
      // Not under this id.
    }
  }
  if (feed?.rows.some(r => r.stage === 'planned') && !(await givernyHookOn($))) w.manager = true
  return w.manager
}

async function readLimit($: EngineInterface, w: LiveWatch, now: number): Promise<void> {
  if (now - w.usageReadAt >= USAGE_READ_MS) {
    w.usageReadAt = now
    try {
      w.limit = spent((await $.session.usage()).rateLimits, now)
    } catch {
      w.limit = undefined
    }
  }
  track(w.holds, w.limit, now)
}

/** Everything but the folds and the clear-done, which are the band's own state. */
export async function readAround($: EngineInterface, w: LiveWatch, session: string, feed: Feed | undefined, agents: AgentRow[], now: number): Promise<Omit<Around, 'folded' | 'dismissed'>> {
  const dir = await feedDir($)
  const sessions = [...new Set([session, ...(feed?.session ? [feed.session] : []), ...(feed?.aliases ?? [])])]
  const ledgerFile = (await $.env.get('GIVERNY_LEDGER')) || `${dir}/resources/ledger.json`
  const ledgerDir = ledgerFile.slice(0, ledgerFile.lastIndexOf('/'))
  const config = (await $.env.get('XDG_CONFIG_HOME')) || `${await $.env.get('HOME')}/.config`
  const ledgerText = await textOf($, w, ledgerFile, 0, now)
  const ledger: Ledger | undefined = ledgerText === undefined ? undefined : parseLedger(ledgerText, now)
  const etas = await textOf($, w, `${dir}/agent-etas/${session}.json`, SHELLS_READ_MS, now)
  const sleeps = await textOf($, w, `${dir}/sleeps.jsonl`, SLOW_READ_MS, now)
  const toml = await textOf($, w, `${config}/giverny/config.toml`, SLOW_READ_MS, now)
  await readLimit($, w, now)
  return {
    manager: await readManager($, w, dir, feed, sessions, now),
    sessions,
    ledger,
    runs: await readRuns($, w, ledgerDir, sessions, now),
    etas: etas === undefined ? {} : parseEtas(etas),
    shells: await readShells($, w, dir, session, agents, now),
    sleeps: sleeps === undefined ? [] : parseSleeps(sleeps),
    holds: w.holds,
    limit: w.limit,
    doneRows: toml === undefined ? 'all' : parseDoneRows(toml),
  }
}
