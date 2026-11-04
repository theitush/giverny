import type { EngineInterface, RenderInput } from 'claude-code'

import type { DisplayRow, Model, Section } from '../types'

import { cells, cellsIn, cut, cutExact, padEnd, padStart } from './cells'
import { gb } from './lease'
import { redrawSoon } from './poll'
import { readHidden, readPage, readSaid, readView, toggleFold } from './state'
import { type Chrome, chromeOf, HOVER_TINT, loadInk, STAGE, STAGE_INK, tint, VIEWED_TINT } from './theme'
import { backWords, drawBox, goBack, openRow } from './worker'

// The band is a copy of Giverny's management panel, cell by cell as far as a terminal grid allows:
// crates/app/src/management_panel.rs `Cols`, `fit_title`, `usage_cell`, `usage_inks` and
// `draw_table`. A column has the panel's width, the whole row is its stage's colour, there are no
// headings, and what does not fit runs off the right. When this file and that one differ, that
// one is right. All that is not the panel's is the back button on the rule and RESERVED.

const STAGE_W = 7
const EL_W = 8
const ETA_W = 10
const NOW_W = 28
const TOK_W = 6
const GAP = 2
const MIN_TITLE = 12
const NEST_W = 3
const TITLE_KEEP = 6
/** How many cells in the table starts: where Claude Code's status line starts. */
const INSET = 2
const OOM_SLOT = 'OOM '
const SEP = '  ·  '
const CPU_W = 8
const USE_MEM_W = 4
/** The rows Claude Code adds under the prompt over a worker's view, `○ main` and a blank, which no plugin can stop. */
export const RESERVED = 2
const STAGE_WORD: Record<Section, string> = { running: 'Running', next: 'NextUp', done: 'Done' }

/** `[management_panel.columns]` of the doc, the manifest's `userConfig`. */
export type Switches = { stage: boolean; id: boolean; title: boolean; elapsed: boolean; eta: boolean; now: boolean; tokens: boolean; usage: boolean }
export const ALL_ON: Switches = { stage: true, id: true, title: true, elapsed: true, eta: true, now: true, tokens: true, usage: true }

/** The manifest's fields into switches; a field not there is on. */
export function switchesOf(options: Readonly<Record<string, unknown>>): Switches {
  const on = (name: keyof Switches) => options[`column_${name}`] !== false
  return { stage: on('stage'), id: on('id'), title: on('title'), elapsed: on('elapsed'), eta: on('eta'), now: on('now'), tokens: on('tokens'), usage: on('usage') }
}

/** A row's use in its cell, `usage_cell`: ` 14% CPU  ·  4.2G`, a Done row's peak in the same place, `OOM` at its head. */
export function usageCell(r: DisplayRow): string {
  if (r.own === 'next') return ''
  const head = r.oom ? OOM_SLOT : ' '.repeat(OOM_SLOT.length)
  const memory = padStart(gb(r.use?.memMb ?? 0), USE_MEM_W)
  if (r.own === 'done') return `${head}${' '.repeat(CPU_W + SEP.length)}${memory}`
  const cpu = padStart(`${Math.min(100, r.use?.cpuPct ?? 0)}% CPU`, CPU_W)
  return `${head}${cpu}${SEP}${memory}`
}

/** Where each column of a row starts or ends, `Cols`: a column switched off takes no width and TASK takes what is left. */
export type Layout = { on: Switches; idw: number; taskw: number; xTask: number; xElEnd: number; xEtaEnd: number; xNow: number; xTokEnd: number; usew: number; xUse: number }

export function layout(rows: DisplayRow[], cols: number, on: Switches = ALL_ON): Layout {
  const most = (of: (r: DisplayRow) => number) => Math.max(0, ...rows.map(of))
  const idw = on.id ? most(r => cells(r.key)) : 0
  const usew = on.usage ? most(r => cells(usageCell(r))) : 0
  const right: [boolean, number][] = [[on.elapsed, EL_W], [on.eta, ETA_W], [on.now, NOW_W], [on.tokens, TOK_W], [usew > 0, usew]]
  const fixed = right.reduce((n, [shown, w]) => n + (shown ? w + GAP : 0), 0)
  const stage = on.stage ? STAGE_W + GAP : 0
  // However narrow, TASK holds the id, each row's nesting, a few cells of title and a folded row's
  // short note, so nothing spills into ELAPSED: the table runs off the right instead.
  const least = most(r => {
    const note = r.fold?.shortNote ?? ''
    return cells(r.head) + (note ? cells(note) + GAP : 0) + TITLE_KEEP
  })
  const taskw = on.title ? Math.max(cols - stage - fixed, (idw > 0 ? idw + 1 : 0) + Math.max(MIN_TITLE, least)) : idw
  let x = 0
  let any = false
  const place = (shown: boolean, w: number) => {
    if (!shown) return x
    const at = any ? x + GAP : 0
    any = true
    x = at + w
    return at
  }
  place(on.stage, STAGE_W)
  const xTask = place(taskw > 0, taskw)
  const xElEnd = place(on.elapsed, EL_W) + EL_W
  const xEtaEnd = place(on.eta, ETA_W) + ETA_W
  const xNow = place(on.now, NOW_W)
  const xTokEnd = place(on.tokens, TOK_W) + TOK_W
  const xUse = Math.max(place(usew > 0, usew), cols - usew)
  return { on, idw, taskw, xTask, xElEnd, xEtaEnd, xNow, xTokEnd, usew, xUse }
}

/** The cells TASK leaves the title, past the id; undefined with the title off. */
const titleW = (l: Layout) => (!l.on.title ? undefined : l.idw > 0 ? Math.max(0, l.taskw - l.idw - 1) : l.taskw)

/** The column of a row's fold toggle, when it has one and the title is shown. */
export function foldX(r: DisplayRow, l: Layout): number | undefined {
  if (!r.fold || !l.on.title) return undefined
  return l.xTask + (l.idw > 0 ? l.idw + 1 : 0) + NEST_W * r.depth
}

/** A row's title cell, `fit_title`: as it narrows the title gives way first, then the note shortens, then it drops. */
export function fitTitle(r: DisplayRow, w: number): string {
  const hw = cells(r.head)
  const keep = Math.min(cells(r.title), TITLE_KEEP)
  for (const note of r.fold?.folded ? [r.fold.note, r.fold.shortNote] : []) {
    if (note === '') continue
    const tail = `  ${note}`
    const used = hw + cells(tail)
    if (used + keep <= w) return `${r.head}${cutExact(r.title, w - used)}${tail}`
  }
  return cutExact(`${r.head}${cutExact(r.title, Math.max(0, w - hw))}`, w)
}

/** The row's cells as `Cols::segments` gives them: `[column, text, colour]`, a use cell in runs of one colour. */
function segments(r: DisplayRow, l: Layout, c: Chrome): [number, string, string][] {
  const { on } = l
  const ink = STAGE_INK[r.own]
  const out: [number, string, string][] = []
  const w = titleW(l)
  if (on.stage) out.push([0, STAGE_WORD[r.own], ink])
  if (l.taskw > 0) {
    const task = w === undefined ? r.key : l.idw > 0 ? `${padEnd(r.key, l.idw)} ${fitTitle(r, w)}` : fitTitle(r, w)
    out.push([l.xTask, task, ink])
  }
  if (on.elapsed) out.push([l.xElEnd - cells(r.elapsed), r.elapsed, ink])
  if (on.eta) out.push([l.xEtaEnd - cells(r.left), r.left, r.noEta ? c.dim : ink])
  // A row with no tokens (Next up) lends NOW the TOKENS column.
  if (on.now) out.push([l.xNow, cutExact(r.what, r.tokens === '' && on.tokens ? NOW_W + GAP + TOK_W : NOW_W), r.flag ? c.amber : ink])
  if (on.tokens) out.push([l.xTokEnd - cells(r.tokens), r.tokens, ink])
  if (l.usew > 0) {
    const cell = usageCell(r)
    const own = r.oom ? c.amber : ink
    const u = r.use
    if (u?.cpuPct === undefined) out.push([l.xUse, cell, own])
    else {
      // The figures each by their weight on the machine; the same columns, so nothing moves.
      const cpuAt = OOM_SLOT.length
      const memAt = cpuAt + CPU_W + SEP.length
      out.push([l.xUse, cell.slice(0, cpuAt), own])
      out.push([l.xUse + cpuAt, cell.slice(cpuAt, cpuAt + CPU_W), loadInk(u.cpuLoad, own, c)])
      out.push([l.xUse + cpuAt + CPU_W, SEP, own])
      out.push([l.xUse + memAt, cell.slice(memAt), loadInk(u.memLoad, own, c)])
    }
  }
  return out
}

/** A run of one colour. */
export type Run = { text: string; ink: string }

/** A row as drawn in `cols` cells, in runs of one colour: laid over each other as `compose`, cut at the right edge. */
export function rowRuns(r: DisplayRow, l: Layout, c: Chrome, cols: number): Run[] {
  const glyphs: string[] = new Array(cols).fill(' ')
  const inks: string[] = new Array(cols).fill(STAGE_INK[r.own])
  for (const [at, text, ink] of segments(r, l, c)) {
    cellsIn(text).forEach((g, i) => {
      const k = at + i
      if (k < 0 || k >= cols) return
      glyphs[k] = g
      if (g !== ' ') inks[k] = ink
    })
  }
  const runs: Run[] = []
  glyphs.forEach((g, k) => {
    const last = runs[runs.length - 1]
    if (last && (last.ink === inks[k] || g === ' ')) last.text += g
    else runs.push({ text: g, ink: inks[k]! })
  })
  return runs
}

/** A row's text, one character a cell, with nothing after its last word: what a test reads. */
export const rowText = (runs: Run[]) => runs.map(s => s.text).join('').trimEnd()

/** Runs cut at a column, for the fold toggle's own press. */
export function cutRuns(runs: Run[], at: number): [Run[], Run[]] {
  const before: Run[] = []
  const after: Run[] = []
  let x = 0
  for (const s of runs) {
    const gs = cellsIn(s.text)
    const left = gs.slice(0, Math.max(0, at - x)).join('')
    const right = gs.slice(Math.max(0, at - x)).join('')
    if (left) before.push({ text: left, ink: s.ink })
    if (right) after.push({ text: right, ink: s.ink })
    x += gs.length
  }
  return [before, after]
}

/** A row's hover: its whole title (its id with none), then why it is where it is, each cut at `room`. */
export function tipLines(r: DisplayRow, room: number): string[] {
  return [r.title || r.key, ...r.why].filter(l => l !== '').map(l => cut(l, room))
}

const THEME_EVERY_MS = 30_000
let theme: { chrome: Chrome; atMs: number } | undefined

/** Claude Code's `theme` setting, for the panel's chrome, looked at now and then: a band redraws as its clock ticks. A read that fails is a dark terminal. */
async function readChrome($: EngineInterface): Promise<Chrome> {
  const now = await $.clock.now()
  if (theme && now - theme.atMs < THEME_EVERY_MS) return theme.chrome
  let chrome = chromeOf(undefined)
  try {
    chrome = chromeOf(await $.settings.read({ key: 'theme' }))
  } catch {
    // Dark.
  }
  theme = { chrome, atMs: now }
  return chrome
}

/** The band's tree, or undefined with nothing to show. */
export async function drawBand($: EngineInterface, e: RenderInput<'AbovePrompt'>, on: Switches = ALL_ON) {
  const model: Model = await readView($)
  const words = await readSaid($)
  const page = await readPage($)
  const box = await drawBox($, e)
  const view = e.props.view
  const viewing = view?.agentId !== undefined
  if (e.props.hasSurvey || (await readHidden($)) || (model.rows.length === 0 && !viewing && !page)) return undefined
  const { Box, Text, Button, Client } = $.ui.resolve(e)
  const chrome = await readChrome($)
  const width = Math.max(20, e.props.bodyColumns)
  const cols = Math.max(1, width - INSET)
  const table = layout(model.rows, Math.max(40, cols), on)

  const fold = async (id: string) => {
    await toggleFold($, id)
    await redrawSoon()
  }

  // Every row's hover reads its whole title and why it is where it is, `Line::tip`: under the
  // row, or over it where the band would clip it. Each is drawn after every row, as what is drawn
  // later paints over what is before it, and shows by the row's hover group. It moves nothing.
  const tip = (r: DisplayRow, i: number) => {
    const lines = tipLines(r, cols - 2)
    if (lines.length === 0) return undefined
    const at = i + 1
    const fits = at + 1 + lines.length <= model.rows.length + 1 + (model.footer ? 1 : 0)
    return (
      <Box position="absolute" top={2} left={INSET} display="none" hover={{ display: 'flex', scope: `row-${r.id}` }} flexDirection="column" backgroundColor={chrome.tip}>
        <Client key={`tip-${r.id}`} module="./tip-client.tsx" props={{ lines }} width={80} height={2} />
      </Box>
    )
  }

  const row = (r: DisplayRow) => {
    const ink = STAGE_INK[r.own]
    const lit = page?.rowId === r.id || (viewing && r.workerId === view?.agentId)
    const bg = lit ? tint(STAGE[r.own], chrome.bg, VIEWED_TINT) : undefined
    const runs = rowRuns(r, table, chrome, cols)
    const drawn = (list: Run[]) => list.map(s => <Text color={s.ink} backgroundColor={bg}>{s.text}</Text>)
    const pressed = (list: Run[], key: string) => (
      <Button key={key} plain onPress={() => openRow($, r, view, e.requestId)}>
        {drawn(list)}
      </Button>
    )
    const at = foldX(r, table)
    let cellsOfRow = [pressed(runs, `row-${r.id}`)]
    const toggler = r.fold
    if (toggler && at !== undefined && at < cols) {
      // A row with agents under it: its toggle, the glyph and the space after it, folds them;
      // the rest of it opens its worker. Keep the indent's cells with the row.
      const [before, rest] = cutRuns(runs, at)
      const [glyph, after] = cutRuns(rest, 2)
      cellsOfRow = [
        pressed(before, `row-${r.id}`),
        <Button key={`fold-${r.id}`} plain onPress={() => fold(toggler.id)}>
          {drawn(glyph)}
        </Button>,
        pressed(after, `rest-${r.id}`),
      ]
    }
    return (
      <Box key={`box-${r.id}`} flexDirection="row" hover={{ scope: `row-${r.id}`, ...(lit ? {} : { backgroundColor: tint(STAGE[r.own], chrome.bg, HOVER_TINT) }) }}>
        {/* The bar at the left edge of the row on view, in the margin, clear of the text. */}
        <Text color={ink}>{lit ? ' ▌' : '  '}</Text>
        {cellsOfRow}
      </Box>
    )
  }

  // The panel's separator, and on it, always here, the way back while a worker is on screen.
  const open = viewing || Boolean(page)
  const label = width < 40 ? '↺ back' : width < 60 ? '↺ manager' : '↺ back to manager'
  const pillW = cells(`0:  ${label} `)
  const said = open ? '' : words
  const saying = open ? backWords(model, view, page, width - INSET - pillW - GAP - GAP - (words ? cells(words) + GAP : 0), words) : cut(said, width - INSET - GAP)
  const used = open ? INSET + pillW + (saying ? GAP + cells(saying) + GAP : GAP) : saying ? GAP + cells(saying) + GAP : 0
  const rule = (
    <Box flexDirection="row">
      {open && <Text>{' '.repeat(INSET)}</Text>}
      {open && (
        <Button key="back" hotkey="0" plain onPress={() => goBack($, view)}>
          <Text color={chrome.bg} backgroundColor={chrome.accent}>{` ${label} `}</Text>
        </Button>
      )}
      {saying !== '' && <Text color={chrome.dim}>{`${' '.repeat(GAP)}${saying}${' '.repeat(GAP)}`}</Text>}
      {open && saying === '' && <Text>{' '.repeat(GAP)}</Text>}
      <Text color={chrome.rule}>{'─'.repeat(Math.max(0, width - used))}</Text>
    </Box>
  )

  return (
    <Box flexDirection="column">
      {box}
      <Box flexDirection="column">
        {rule}
        {model.rows.map(row)}
        {model.footer ? <Text color={chrome.dim} wrap="truncate">{`${' '.repeat(INSET)}${cutExact(model.footer, cols)}`}</Text> : undefined}
        {model.rows.map(tip)}
      </Box>
      {/* Claude Code's own rows under the prompt come over a worker's view and push the band up by
          as many: this room gives way to them, so the table stays on its screen lines. */}
      {!viewing && model.rows.some(r => r.workerId !== undefined) && <Box height={RESERVED} />}
    </Box>
  )
}
