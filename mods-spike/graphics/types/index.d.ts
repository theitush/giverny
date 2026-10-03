export type AgentsPaneSpikeRow = {
  key: string
  stage: 'running' | 'planned' | 'done'
  title: string
  startedMs?: number
  endedMs?: number
  etaS?: number
  tokens?: number
  now?: string
  outcome?: 'landed' | 'blocked' | 'review'
  review?: string
  lease?: string
  usage?: string
  brief: string
}

declare module 'claude-code' {
  interface PluginState {
    'agents-pane-spike': {
      rows: AgentsPaneSpikeRow[]
      now: number
      open: string | null
      briefKey: string | null
      openNote: string
    }
  }
}
