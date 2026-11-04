// The plugin's shared shapes. FeedRow is the whole row of docs/management-panel.md, so a
// new drawing reads the field it wants without changing this file.

export type Stage = 'running' | 'planned' | 'done'

/** A row as src/feed.ts hands it on: every time is epoch ms, every count a whole number. */
export type FeedRow = {
  key: string
  stage: Stage
  title?: string
  agent_id?: string
  started?: number
  ended?: number
  spawned?: number
  paused_s?: number
  paused_since?: number
  killed?: number
  went_off?: number
  eta_s?: number
  eta_first_s?: number
  eta_delta_s?: number
  left_s?: number
  left_at?: number
  eta_at?: number
  wait_s?: number
  waiting_since?: number
  reest_s?: number
  reest_at_s?: number
  /** The raw guess a row from before giverny#229 kept: the first thing a landing is measured against. */
  eta_guess_s?: number
  repo?: string
  landing?: string
  tokens?: number
  task_tokens?: number
  group?: string
  brief?: string
  open?: string
  note?: string
  review?: string
  follows_worker?: boolean
  planned?: number
  after?: string
  held?: string
  unheld?: number
  lease?: FeedLease
  usage?: FeedUsage
  /** When the manager was last told of this row (giverny#310): a chore told is not told again. */
  land_nudged?: number
  start_nudged?: number
  worker_nudged?: number
  /** A `giverny-manage run` of this row is going, from the ledger's run records: not in the file. */
  live?: boolean
}

/** A row's `lease`, normalised by src/feed.ts. For a queued row the figures are the request. */
export type FeedLease = {
  state: 'granted' | 'smaller' | 'queued'
  ram_mb: number
  gpus: number[]
  vram_mb: number
  slots: string[]
  id?: string
  granted_ms?: number
  wanted_ram_mb?: number
  position?: number
  behind?: string
  /** `cpu`, `ram`, `gpu`, `slot <name>`: what it is short of, as of its last claim. */
  short: string[]
  /** Granted on another session's call, not yet taken up by its manager. */
  uncollected: boolean
  /** Queued again after such a grant lapsed untaken. */
  lapsed: boolean
}

export type FeedUsage = {
  runs?: number
  peak_mb?: number
  cpu_s?: number
  wall_s?: number
  oom_kills?: number
  oom_open?: boolean
  last_cmd?: string
  last_exit?: number
  last_peak_mb?: number
  last_at?: string | number
  capped?: boolean
  cap_ram_mb?: number
}

export type Feed = {
  session?: string
  aliases: string[]
  name?: string
  writer?: string
  footer?: string
  rows: FeedRow[]
  /** The file's mtime: a paused row's `started` is only true as of then, for a writer that pauses that way. */
  writtenMs?: number
  /** Off-screen tests only (dev/look.sh sets MANAGER_MOD_DEV): stand-in workers. */
  dev_agents?: AgentRow[]
}

export type LedgerLease = {
  id: string
  session: string
  task: string
  ram_mb: number
  gpus: number[]
  vram_mb: number
  slots: string[]
  granted_ms?: number
  uncollected: boolean
}

export type LedgerWaiter = {
  session: string
  task: string
  ram_mb: number
  gpu: number
  vram_mb: number
  slots: string[]
  priority?: string
  queued_ms: number
  lapsed: boolean
}

/** The machine ledger: <feed dir>/resources/ledger.json, less what has expired. */
export type Ledger = { leases: LedgerLease[]; queue: LedgerWaiter[] }

/** A span a worker stood stopped on an API error, and why: `no network`, `usage limit`. */
export type Stop = { fromMs: number; toMs?: number; reason: string }

/** `$.agent.list()` with what the worker's transcript adds, which that list does not give. */
export type AgentRow = {
  id: string
  description: string
  status: string
  parentId?: string
  name?: string
  /** Its first turn. */
  startedMs?: number
  /** Its transcript's last line, once it has finished. */
  endedMs?: number
  lastTurnMs?: number
  /** The context of its last turn. */
  tokens?: number
  /** Its last tool call, `Bash: cargo test`. */
  activity?: string
  model?: string
  stops?: Stop[]
  transcript?: string
  /** Off-screen tests only: its background shells, as bgshells/<session>.json would say. */
  shells?: Shell[]
}

/** A worker's own estimate: <feed dir>/agent-etas/<session>.json. */
export type AgentEta = { leftS: number; atMs: number; firstLeftS: number; firstAtMs: number }

/** A background shell a worker started that still runs. */
export type Shell = { id: string; cmd: string; startedMs: number }

/** A stretch nothing ran: the machine slept, or the account was out of its usage. */
export type Span = { since: number; until: number }

/** One span the account could run nothing; `untilMs` is absent while it is still out. */
export type Hold = { sinceMs: number; untilMs?: number; reopensMs?: number }

/** The window used up: `5h`, `7d`, and when it reopens. */
export type Limit = { window?: string; reopensMs?: number }

/** `management_panel.done_rows`: all, none, or the newest N. */
export type DoneRows = 'all' | 'hide' | number

/** Everything a row's words read beyond the feed and the workers. */
export type Around = {
  /** The manage skill ran in this session: its Next up and build rows draw with no worker. */
  manager: boolean
  /** This session's ids, for matching the ledger's entries. */
  sessions: string[]
  /** The ledger; undefined when it cannot be read, and the rows' own copies stand. */
  ledger?: Ledger
  /** Keys of the rows with a `giverny-manage run` going. */
  runs: string[]
  etas: Record<string, AgentEta>
  /** By agent id: the shells it started that still run. */
  shells: Record<string, Shell[]>
  sleeps: Span[]
  holds: Hold[]
  limit?: Limit
  doneRows: DoneRows
  /** Rows whose agents are folded away, by DisplayRow.fold.id. */
  folded: string[]
  /** `/manager clear-done`: Done rows that landed by then are hidden, whoever wrote them. */
  clearedMs?: number
  /** And the finished workers it cleared. */
  dismissed: string[]
}

export type Section = 'running' | 'next' | 'done'

/** The toggle on a row whose agent spawned agents of its own. */
export type RowFold = {
  /** What to pass to toggleFold(). */
  id: string
  folded: boolean
  /** How many hang under it, every level down, and how many of those run. */
  under: number
  running: number
  /** `+6, 2 running` while folded; nothing while open. */
  note: string
  /** `+6`, for a narrow row. */
  shortNote: string
}

/** One line of the band: all it draws, as text, with the worker a press opens. */
export type DisplayRow = {
  /** Stable across redraws: the feed key, or the agent id of a worker with no row. */
  id: string
  /** Where it is drawn: an agent under another's row goes where that row goes. */
  section: Section
  /** Its own stage, which a nested row may not share with its section: draw it in this colour. */
  own: Section
  /** The TASK column: the task's key; for a worker no feed row carries, its name or model, or nothing. */
  key: string
  title: string
  /** What the title is drawn behind: indent and `▾ `/`▸ `/`└ `, or the toggle's room. */
  head: string
  /** How deep under another row it hangs: 0 for a row of its own. */
  depth: number
  fold?: RowFold
  /** ELAPSED: `14m`; on a ditto, `"`. */
  elapsed: string
  /** ETA: `~20m`, `-~12m` past it, on Next up `~10m`, on Done `(+5m)`, or `no ETA`. */
  left: string
  /** `left` is past its estimate. */
  over: boolean
  /** `left` is `no ETA`: draw it dim. */
  noEta: boolean
  /** NOW: what it does, what it waits for, or how it landed; on a ditto, `"`. */
  what: string
  /** `what` is for the person to know: draw it in the warning colour. */
  flag: boolean
  /** TOKENS, as text; on a ditto, `"`. */
  tokens: string
  /** A `giverny-manage run` of it was killed by its memory cap, and that is still news. */
  oom: boolean
  /** The hover: the whole title first, then where the row is and why. */
  why: string[]
  /** The header over its worker page: `took 12m`, `(-5m) vs worker's 30m`, `holds 3G`, `peak 2.1G of 3G`. */
  facts: string[]
  /** What the machine ledger says of it, in full: `holds 3G, slot x`, `queued #2 for 3G behind y`. */
  lease: string
  /** Done rows: the line a person has to read before it counts. */
  review?: string
  /** When a Done row landed. */
  landedMs?: number
  /** The live worker behind the row. */
  workerId?: string
  /** Its worker's turn has ended: nothing to walk into; open its transcript instead. */
  finished: boolean
  /** Its worker sleeps on a background shell of its own. */
  asleep: boolean
  transcript?: string
  brief?: string
  open?: string
  note?: string
}

export type Model = {
  rows: DisplayRow[]
  running: number
  next: number
  done: number
  /** The feed's footer, verbatim. */
  footer?: string
  /** Some row has agents hung under it: every title leaves the toggle's room. */
  nested: boolean
}

declare module 'claude-code' {
  interface PluginState {
    manager: {
      view: Shaped<Model>
      hidden: boolean
      said: string
      folded: Shaped<string[]>
      cleared: Shaped<{ atMs?: number; agents: string[] }>
    }
  }
}
