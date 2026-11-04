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
}

export type FeedLease = {
  state?: string
  ram_mb?: number
  gpus?: number[]
  vram_mb?: number
  slots?: string[]
  id?: string
  granted_at?: string | number
  wanted_ram_mb?: number
  position?: number
  behind?: string
  short?: string
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
}

/** What the model needs of `$.agent.list()`. */
export type AgentRow = {
  id: string
  description: string
  status: string
  parentId?: string
}

export type Section = 'running' | 'next' | 'done'

/** One line of the band: all it draws, as text, with the worker a press opens. */
export type DisplayRow = {
  /** Stable across redraws: the feed key, or the agent id of a worker with no row. */
  id: string
  section: Section
  /** The task's key; empty for a worker no feed row carries. */
  key: string
  title: string
  elapsed: string
  /** `~8m left`, `over by 3m`, `paused`, `~10m`, `est 12m`, or nothing. */
  left: string
  over: boolean
  /** The NOW column: note, what it waits for, or how it landed. */
  what: string
  /** The live worker behind the row. */
  workerId?: string
}

export type Model = {
  rows: DisplayRow[]
  running: number
  next: number
  done: number
}

declare module 'claude-code' {
  interface PluginState {
    manager: {
      view: Shaped<Model>
      hidden: boolean
      said: string
    }
  }
}
