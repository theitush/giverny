// agents-pane-spike (giverny#186): draws Giverny's agents pane from fake
// data in a mod Pane, to measure whether a mod's graphics are enough.
import { atom, read, update } from 'claude-code'
import type { Register } from 'claude-code'

import type { AgentsPaneSpikeRow as Row } from '../types'

const PANE = 'agents'
const BRIEF = 'agents-brief'

const rows = atom({ plugin: 'agents-pane-spike', key: 'rows' } as const, [])
const now = atom({ plugin: 'agents-pane-spike', key: 'now' } as const, 0)
const open = atom({ plugin: 'agents-pane-spike', key: 'open' } as const, null)
const briefKey = atom({ plugin: 'agents-pane-spike', key: 'briefKey' } as const, null)
const openNote = atom({ plugin: 'agents-pane-spike', key: 'openNote' } as const, '')

// orchestrate-status / agents_pane.rs stage tints
const C = {
  running: '#0087d7',
  planned: '#5f87af',
  done: '#008700',
  blocked: '#d75f5f',
  review: '#d7af00',
  over: '#d75f00',
}

const MIN = 60_000

function fakeRows(t: number): Row[] {
  const brief = (k: string, what: string) =>
    `# ${k}\n\n**Ask** — ${what}\n\n- read the issue\n- do the work in a worktree\n- \`cargo-shared test\` then push\n\n*brief: /tmp/briefs/${k.replace('#', '-')}.md*`
  return [
    { key: 'giverny#183', stage: 'running', title: 'BUG: pane: ETA counts past zero', startedMs: t - 12 * MIN - 7_000, etaS: 30 * 60, tokens: 64_100, now: 'Bash cargo-shared test', lease: '3 cpu · 3G', usage: '2.1 cpu 1.4G', brief: brief('giverny#183', 'ETA holds at zero') },
    { key: 'inbar#616', stage: 'running', title: 'RESEARCH: backtest(#61): slippage sweep over 2024', startedMs: t - 41 * MIN, etaS: 40 * 60, tokens: 212_400, now: 'Edit sweep.py', lease: '2 cpu · 6G', usage: '1.9 cpu 5.2G', brief: brief('inbar#616', 'sweep slippage') },
    { key: 'coo#170', stage: 'running', title: 'FEATURE: board: pause a pass', startedMs: t - 3 * MIN - 30_000, etaS: undefined, tokens: 9_800, now: 'Read board.ts', brief: brief('coo#170', 'pause a pass') },
    { key: 'giverny#184', stage: 'planned', title: 'CLEANUP: feed: drop the v1 reader', etaS: 25 * 60, now: 'queued: 2nd for 3G', brief: brief('giverny#184', 'drop v1 reader') },
    { key: 'planets#88', stage: 'planned', title: 'FEATURE: editor: export SVG at poster size', etaS: 63 * 60, brief: brief('planets#88', 'SVG export') },
    { key: 'giverny#179', stage: 'done', title: 'BUG: tabs: title flickers on resume', startedMs: t - 95 * MIN, endedMs: t - 60 * MIN, etaS: 30 * 60, tokens: 156_313, outcome: 'landed', usage: 'peak 2.8G', brief: brief('giverny#179', 'title flicker') },
    { key: 'leadgen#41', stage: 'done', title: 'RUN: outreach: send batch 7', startedMs: t - 80 * MIN, endedMs: t - 70 * MIN, etaS: 20 * 60, tokens: 31_000, outcome: 'blocked', now: 'Blocked — SMTP creds expired', brief: brief('leadgen#41', 'send batch 7') },
    { key: 'giverny#35', stage: 'done', title: 'FEATURE: theme: catppuccin-mauve', startedMs: t - 140 * MIN, endedMs: t - 85 * MIN, etaS: 60 * 60, tokens: 488_000, outcome: 'review', review: 'Review: ita — look at the mauve accent, branch theme-mauve', brief: brief('giverny#35', 'mauve theme') },
  ]
}

function stopwatch(s: number): string {
  s = Math.max(0, Math.floor(s))
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = s % 60
  const p = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${p(m)}:${p(x)}` : `${m}:${p(x)}`
}
function dur(s: number): string {
  const m = Math.round(Math.abs(s) / 60)
  return m >= 60 ? `${Math.floor(m / 60)}h${m % 60 ? (m % 60) + 'm' : ''}` : `${m}m`
}
function countdown(left: number): string {
  if (left >= 0) return left < 60 ? `${Math.floor(left)}s` : `~${dur(left)}`
  return `+${dur(left)}`
}
function tokens(n?: number): string {
  if (n === undefined) return ''
  if (n < 1000) return String(n)
  if (n < 999_950) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`
  return `${(n / 1e6).toFixed(1).replace(/\.0$/, '')}M`
}
function pad(s: string, w: number, right = false): string {
  if (s.length > w) s = s.slice(0, Math.max(0, w - 1)) + '…'
  return right ? s.padStart(w) : s.padEnd(w)
}

type Cells = { stage: string; color: string; elapsed: string; eta: string; etaColor?: string; now: string; nowColor?: string; tok: string; frac?: number; barColor: string }

function cells(r: Row, t: number): Cells {
  if (r.stage === 'running') {
    const el = (t - (r.startedMs ?? t)) / 1000
    const left = r.etaS === undefined ? undefined : r.etaS - el
    return {
      stage: 'Running', color: C.running, elapsed: stopwatch(el),
      eta: left === undefined ? 'no ETA' : countdown(left),
      etaColor: left === undefined ? undefined : left < 0 ? C.over : undefined,
      now: r.now ?? '', tok: tokens(r.tokens),
      frac: r.etaS === undefined ? undefined : el / r.etaS, barColor: left !== undefined && left < 0 ? C.over : C.running,
    }
  }
  if (r.stage === 'planned') {
    return { stage: 'NextUp', color: C.planned, elapsed: '', eta: r.etaS ? `~${dur(r.etaS)}` : '', now: r.now ?? '', tok: '', barColor: C.planned }
  }
  const took = ((r.endedMs ?? 0) - (r.startedMs ?? 0)) / 1000
  const delta = r.etaS === undefined ? '' : `(${took >= r.etaS ? '+' : '-'}${dur(took - r.etaS)})`
  const word = r.outcome === 'blocked' ? 'Blocked' : r.outcome === 'review' ? 'Review' : 'landed'
  const col = r.outcome === 'blocked' ? C.blocked : r.outcome === 'review' ? C.review : C.done
  return { stage: 'Done', color: C.done, elapsed: stopwatch(took), eta: delta, now: r.now ?? word, nowColor: col, tok: tokens(r.tokens), frac: 1, barColor: col }
}

function bar(frac: number | undefined, w: number): [string, string] {
  if (frac === undefined) return ['', '·'.repeat(w)]
  const f = Math.max(0, Math.min(1, frac))
  const full = Math.round(f * w)
  return ['█'.repeat(full), '░'.repeat(w - full)]
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const t = await $.clock.now()
    await update($, rows, () => fakeRows(t))
    await update($, now, () => t)
    // Capture aid: a test run with no way to press a row can start with one open.
    const pre = await $.env.get('AGENTS_SPIKE_OPEN')
    if (pre) await update($, open, () => pre)
    await $.command.register({ name: 'agents-pane', description: 'Open the agents pane (giverny#186 spike, fake data)' })
    $.clock.every(1000, () => {
      void $.clock.now().then(n => update($, now, () => n))
    })
    // Unasked open: placed only from 144 columns (types: ui.open).
    const res = await $.ui.open({ id: PANE, title: 'Agents' })
    const note = res.isPlaced ? 'opened unasked: placed' : `opened unasked: NOT placed — ${res.reason}`
    await update($, openNote, () => note)
    $.ui.log(`agents-pane-spike: ${note}`)
    return next(e)
  })

  on('command.run', { command: 'agents-pane' }, async $ => {
    const res = await $.ui.open({ id: PANE, title: 'Agents' })
    return { text: res.isPlaced ? 'Agents pane opened.' : `Agents pane waits: ${res.reason}` }
  })

  on('ui.render', { component: 'Pane', requestId: BRIEF }, async ($, e) => {
    const { Box, Text, Markdown, Button } = $.ui.resolve(e)
    const k = await read($, briefKey)
    const r = (await read($, rows)).find(x => x.key === k)
    return (
      <Box flexDirection="column">
        <Markdown text={r ? r.brief : '_no brief selected_'} />
        <Box><Button key="close-brief" label="Close" role="dismiss" onPress={() => $.ui.close({ id: BRIEF })} /></Box>
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text, Button, Markdown } = $.ui.resolve(e)
    const list = await read($, rows)
    const t = await read($, now)
    const opened = await read($, open)
    const note = await read($, openNote)
    const W = e.props.bodyColumns
    const wide = W >= 96

    const section = (label: string, color: string, n: number) => (
      <Box marginTop={0}><Text color={color} bold>{label}</Text><Text dimColor> {n}</Text></Box>
    )

    const rowView = (r: Row) => {
      const c = cells(r, t)
      const isOpen = opened === r.key
      const press = () => update($, open, cur => (cur === r.key ? null : r.key))
      const [done, rest] = bar(c.frac, wide ? 10 : 8)
      const toggle = <Button key={`row:${r.key}`} plain label={`${isOpen ? '▾' : '▸'} ${r.key}`} onPress={press} />
      const barEl = <Text><Text color={c.barColor}>{done}</Text><Text dimColor>{rest}</Text></Text>
      const detail = isOpen && (
        <Box flexDirection="column" borderStyle="round" borderColor={c.color} paddingX={1}>
          {r.review && <Text color={C.review} bold>{r.review}</Text>}
          <Text dimColor>
            {[r.lease && `holds ${r.lease}`, r.usage && `uses ${r.usage}`, c.tok && `${c.tok} tokens`].filter(Boolean).join(' · ') || 'no lease'}
          </Text>
          <Markdown text={r.brief} />
          <Box gap={1}>
            <Button key={`brief:${r.key}`} label="Open brief in a pane" variant="primary" onPress={async () => { await update($, briefKey, () => r.key); await $.ui.open({ id: BRIEF, title: `Brief ${r.key}` }) }} />
            <Button key={`close:${r.key}`} label="Collapse" onPress={press} />
          </Box>
        </Box>
      )
      if (wide) {
        const titleW = Math.max(10, W - (8 + 1 + 14 + 1 + 10 + 1 + 8 + 1 + 8 + 1 + 24 + 1 + 7) - 2)
        return (
          <Box key={`box:${r.key}`} flexDirection="column" hover={{ backgroundColor: '#303030' }}>
            <Box>
              <Text color={c.color} bold={r.stage === 'running'}>{pad(c.stage, 8)} </Text>
              <Box width={14}>{toggle}</Box>
              <Text> {pad(r.title, titleW)} </Text>
              {barEl}
              <Text> {pad(c.elapsed, 8, true)} </Text>
              <Text color={c.etaColor} dimColor={c.eta === 'no ETA'}>{pad(c.eta, 8, true)} </Text>
              <Text color={c.nowColor} dimColor={!c.nowColor}>{pad(c.now, 24)} </Text>
              <Text>{pad(c.tok, 7, true)}</Text>
            </Box>
            {detail}
          </Box>
        )
      }
      return (
        <Box key={`box:${r.key}`} flexDirection="column" hover={{ backgroundColor: '#303030' }}>
          <Box>
            <Text color={c.color} bold={r.stage === 'running'}>{c.stage === 'NextUp' ? '○' : c.stage === 'Done' ? '✓' : '●'} </Text>
            {toggle}
            <Text wrap="truncate-end"> {r.title}</Text>
          </Box>
          <Box paddingLeft={2}>
            {barEl}
            <Text> {c.elapsed}</Text>
            <Text color={c.etaColor} dimColor={c.eta === 'no ETA'}> {c.eta}</Text>
            {c.tok && <Text dimColor> {c.tok}t</Text>}
          </Box>
          {c.now && <Box paddingLeft={2}><Text color={c.nowColor} dimColor={!c.nowColor} wrap="truncate-end">{c.now}</Text></Box>}
          {detail}
        </Box>
      )
    }

    const by = (s: Row['stage']) => list.filter(r => r.stage === s)
    const run = by('running'), next = by('planned'), done = by('done')
    return (
      <Box flexDirection="column">
        {section('Running', C.running, run.length)}
        {run.map(rowView)}
        {section('Next up', C.planned, next.length)}
        {next.map(rowView)}
        {section('Done', C.done, done.length)}
        {done.map(rowView)}
        <Text dimColor wrap="truncate-end">leased 11G/22G RAM · 5/8 cores · body {W} cols · {e.props.placement} · {note}</Text>
      </Box>
    )
  })
}
