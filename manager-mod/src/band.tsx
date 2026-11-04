import type { EngineInterface, RenderInput } from 'claude-code'

import type { DisplayRow, Model, Section } from '../types'

import type { BarPost } from './bar-client'
import type { GripPost } from './grip-client'

import { cells, cellsIn, cut, cutExact, padEnd, padStart } from './cells'
import { gb } from './lease'
import { redrawSoon } from './poll'
import { readHidden, readPage, readSaid, readSize, readTop, readView, readWorkerView, toggleFold, writeSize, writeTop } from './state'
import { type Chrome, chromeOf, holdsHex, HOVER_TINT, loadInk, rowFill, STAGE_INK, VIEWED_TINT } from './theme'
import { backWords, drawBox, goBack, openRow, pagesWorkers, startFromPage } from './worker'

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
const STORE_ROWS = 'band-rows'
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

/**
 * A row's hover, `Line::tip`: its whole title (its id with none), then each reason it is where it
 * is on a line of its own, wrapped at `room` cells. Past `lines` lines, the last is cut at its end.
 */
export function tipLines(r: DisplayRow, room: number, lines: number): string[] {
  const out: string[] = []
  for (const part of [r.title || r.key, ...r.why].filter(l => l !== '')) {
    let rest = part
    while (cells(rest) > room) {
      // At the last space that fits, as a tooltip wraps; a word longer than the line is cut.
      const gs = cellsIn(rest)
      const space = gs.lastIndexOf(' ', room)
      const at = space > 0 ? space : room
      out.push(gs.slice(0, at).join(''))
      rest = gs.slice(space > 0 ? at + 1 : at).join('')
    }
    out.push(rest)
  }
  if (out.length <= lines) return out
  return [...out.slice(0, lines - 1), cut(out.slice(lines - 1).join(' '), room)]
}

/**
 * Where a row's tooltip goes among `[top, bottom)`, what the band shows: under the row at `rowY` as
 * Giverny's, over it where that would be clipped, on whichever side has more room if neither holds
 * `tall` rows, and never on the row itself, which it would take the pointer from.
 */
export function tipPlace(rowY: number, tall: number, top: number, bottom: number): { y: number; room: number } {
  const under = bottom - rowY - 1
  const over = rowY - top
  if (under >= tall || under >= over) return { y: rowY + 1, room: Math.min(under, tall) }
  const room = Math.min(over, tall)
  return { y: rowY - room, room }
}

/** The fewest rows the person can size the band to, under its rule. */
export const MIN_ROWS = 2
const RULE_ROWS = 1
/** A tooltip's width at most, in cells of text: egui's `tooltip_width` of 500 points at the panel's font. */
const TIP_ROOM = 72
/** What its edge and padding take: a row each side, and two cells. */
const TIP_EDGE_ROWS = 2
const TIP_EDGE_CELLS = 4
/** The scroll bar stands in the cells the engine keeps right of the band, under its `[-]`. */
const BAR_GAP = 2

/**
 * What the band shows under its rule: its rows from `top`, `shown` of them, then `blank` empty rows,
 * `area` in all with the footer. `own`: a window of the plugin's, as the engine's cannot be shorter
 * than the whole site.
 */
export type BandWindow = { top: number; shown: number; blank: number; area: number; own: boolean }

/**
 * Never set it is sized to its rows, as the panel's `pane_height`; set, it holds that height
 * whatever the rows do. Past `room` the tree is taller than the site, and the engine scrolls it with
 * its wheel and its `n more`, as it always has; set shorter, what is left is a window of the
 * plugin's, with nothing drawn to say so but the panel's scroll bar.
 */
export function bandWindow(rows: number, footer: number, chosen: number | null, room: number, top: number): BandWindow {
  if (chosen === null || chosen >= room) {
    const area = Math.max(1, Math.min(chosen ?? rows + footer, room))
    return { top: 0, shown: rows, blank: Math.max(0, area - footer - rows), area, own: false }
  }
  const area = Math.max(MIN_ROWS, chosen)
  const shown = Math.min(rows, Math.max(1, area - footer))
  return { top: Math.max(0, Math.min(top, rows - shown)), shown, blank: Math.max(0, area - footer - shown), area, own: shown < rows }
}

/** As the last drawing left them: what a drag and the wheel are clamped to. */
const drawn = { room: 0, lastTop: 0, top: 0, shown: 0, own: false }

/** A height as asked for into one the band keeps: what is whole, and at least the fewest. */
const sane = (n: number) => Math.max(MIN_ROWS, Math.round(n))

/** The height kept from the last session, if there is one. */
export async function loadSize($: EngineInterface): Promise<void> {
  try {
    const kept = await $.store.get(STORE_ROWS)
    if (typeof kept === 'number' && Number.isFinite(kept)) await writeSize($, sane(kept))
  } catch {
    // A store that cannot be read: it follows its rows.
  }
}

/** `/manager size <rows>`, `auto`, as said back to the person. */
export async function sizeCommand($: EngineInterface, arg: string): Promise<string> {
  if (arg === 'auto') {
    await writeSize($, null)
    await $.store.delete(STORE_ROWS)
    return 'The manager band follows its rows again.'
  }
  const n = /^\d+$/.test(arg) ? Number(arg) : undefined
  if (n === undefined) return `Usage: /manager size <rows> | auto. Now ${(await readSize($)) ?? 'auto'}; the most that fits here is ${drawn.room} rows.`
  const rows = sane(n)
  await writeSize($, rows)
  await $.store.set(STORE_ROWS, rows)
  return drawn.room > 0 && rows > drawn.room ? `Manager band set to ${rows} rows; ${drawn.room} is the most that fits here now.` : `Manager band set to ${rows} rows.`
}

/** A move of the rule (src/grip-client.tsx): the height under the pointer's row. Its data came from code, so it is checked. */
export async function dragged($: EngineInterface, data: unknown): Promise<void> {
  const { y, rows, done } = (data ?? {}) as Partial<GripPost>
  if (typeof y !== 'number' || typeof rows !== 'number') return
  const value = sane(Math.min(rows - y, drawn.room))
  await writeSize($, value)
  if (done === true) await $.store.set(STORE_ROWS, value)
}

async function moveTop($: EngineInterface, to: number): Promise<void> {
  const at = Math.max(0, Math.min(to, drawn.lastTop))
  if (at !== drawn.top) {
    drawn.top = at
    await writeTop($, at)
  }
}

/** Whether the band's window is the plugin's, so a scroll is this plugin's to do. */
export const ownWindow = () => drawn.own

/**
 * A scroll key while the band holds the keys, in the plugin's window: an arrow never comes here, as
 * there it walks the focus ring. A page key and Home and End come with the whole tree's rows, which
 * cannot be told apart, so each moves a window's worth.
 */
export async function scrolled($: EngineInterface, by: number): Promise<void> {
  await moveTop($, drawn.top + Math.sign(by) * Math.min(Math.abs(by), drawn.shown))
}

/** A press or a drag on the scroll bar (src/bar-client.tsx), its data checked as it came from code. */
export async function barMoved($: EngineInterface, data: unknown): Promise<void> {
  const { y, rows } = (data ?? {}) as Partial<BarPost>
  if (typeof y !== 'number' || typeof rows !== 'number' || rows < 1) return
  await moveTop($, Math.round((Math.max(0, Math.min(y, rows - 1)) / Math.max(1, rows - 1)) * drawn.lastTop))
}

const THEME_EVERY_MS = 30_000
let theme: { chrome: Chrome; atMs: number } | undefined

let hexHolds: boolean | undefined

/** Whether this terminal gets a hex as it is: its environment doesn't change while Claude Code runs. */
async function readHexHolds($: EngineInterface): Promise<boolean> {
  hexHolds ??= holdsHex({
    colorTerm: await $.env.get('COLORTERM'),
    term: await $.env.get('TERM'),
    termProgram: await $.env.get('TERM_PROGRAM'),
    tmux: await $.env.get('TMUX'),
    tmuxTrueColour: await $.env.get('CLAUDE_CODE_TMUX_TRUECOLOR'),
    forceColor: await $.env.get('FORCE_COLOR'),
  })
  return hexHolds
}

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
  const hex = await readHexHolds($)
  const width = Math.max(20, e.props.bodyColumns)
  const cols = Math.max(1, width - INSET)
  const table = layout(model.rows, Math.max(40, cols), on)

  // The rows the worker's view takes under the prompt are the rows kept here when it is shut: the
  // site's room falls by as many as the tree gives back, so the window is the same either way. Where
  // a worker's row opens the page, that view does not come, and there is nothing to keep.
  const hasWorkers = model.rows.some(r => r.workerId !== undefined)
  const keeps = !viewing && hasWorkers && !pagesWorkers(await readWorkerView($), e.viewport)
  const boxRows = page?.ask && page.place === 'pane' ? 1 + (page.status !== '' ? 1 : 0) : 0
  const footerRows = model.footer ? 1 : 0
  const room = e.props.maxRows - RULE_ROWS - boxRows - (keeps ? RESERVED : 0)
  const open = viewing || Boolean(page)
  const chosen = await readSize($)
  const top = await readTop($)
  const asked = bandWindow(model.rows.length, footerRows, chosen, room, top)
  // The back row grows the band upward, so the rule and the rows stay on their screen lines. A band
  // with no row left to grow into keeps them there too: the back row stands under the table, in the
  // reserved rows where there are any, else in the place of the window's last row.
  const under0 = open && footerRows + asked.shown + asked.blank >= room
  const shorter = under0 && !keeps
  const win = shorter ? bandWindow(model.rows.length, footerRows, Math.max(MIN_ROWS, room - 1), room, top) : asked
  const backRows = open && !under0 ? 1 : 0
  drawn.room = room
  drawn.top = win.top
  drawn.shown = win.shown
  drawn.lastTop = model.rows.length - win.shown
  drawn.own = win.own
  const onRow = model.rows.slice(win.top, win.top + win.shown)
  const thumb = Math.max(1, Math.round((win.shown * win.shown) / Math.max(1, model.rows.length)))
  const thumbAt = drawn.lastTop > 0 ? Math.round((win.top / drawn.lastTop) * (win.shown - thumb)) : 0

  const fold = async (id: string) => {
    await toggleFold($, id)
    await redrawSoon()
  }

  const row = (r: DisplayRow) => {
    const ink = STAGE_INK[r.own]
    const lit = page?.rowId === r.id || (viewing && r.workerId === view?.agentId)
    const bg = lit ? rowFill(r.own, chrome, VIEWED_TINT, hex) : undefined
    const runs = rowRuns(r, table, chrome, cols)
    const drawn = (list: Run[]) => list.map(s => <Text color={s.ink} backgroundColor={bg}>{s.text}</Text>)
    const pressed = (list: Run[], key: string) => (
      <Button key={key} plain hover={{ scope: `row-${r.id}`, inverse: false }} onPress={() => openRow($, r, view, e.requestId, e.viewport)}>
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
        <Button key={`fold-${r.id}`} plain hover={{ scope: `row-${r.id}`, inverse: false }} onPress={() => fold(toggler.id)}>
          {drawn(glyph)}
        </Button>,
        pressed(after, `rest-${r.id}`),
      ]
    }
    return (
      <Box key={`box-${r.id}`} flexDirection="row" hover={{ scope: `row-${r.id}`, ...(lit ? {} : { backgroundColor: rowFill(r.own, chrome, HOVER_TINT, hex) }) }}>
        {/* The bar at the left edge of the row on view, in the margin, clear of the text. */}
        <Text color={ink}>{lit ? ' ▌' : '  '}</Text>
        {cellsOfRow}
      </Box>
    )
  }

  // The panel's separator, edge to edge, and a row of its own for the way back while a worker is on
  // screen: the rule keeps its whole line, for the eye and the drag.
  const label = width < 40 ? '↺ back' : width < 60 ? '↺ manager' : '↺ back to manager'
  const pillW = cells(`0:  ${label} `)
  const saying = cut(open ? '' : words, width - INSET - GAP)
  const ruleW = Math.max(0, width - (saying ? GAP + cells(saying) + GAP : 0))
  const startsHere = page?.place === 'row' && page.ask === 'start'
  const startText = ' ▶ start now '
  const backWordsNow = backWords(model, view, page ?? undefined, width - INSET - pillW - GAP - (startsHere ? GAP + cells(startText) : 0), words, e.props.isWorking)
  const back = open && (
    <Box flexDirection="row">
      <Text>{' '.repeat(INSET)}</Text>
      <Button key="back" hotkey="0" plain onPress={() => goBack($, view)}>
        <Text color={chrome.bg} backgroundColor={chrome.accent}>{` ${label} `}</Text>
      </Button>
      {startsHere && <Text>{' '.repeat(GAP)}</Text>}
      {/* No hotkey: a digit typed at the prompt would start a task. */}
      {startsHere && (
        <Button key="start-now" plain onPress={() => startFromPage($, view)}>
          <Text color={chrome.bg} backgroundColor={STAGE_INK.next}>{startText}</Text>
        </Button>
      )}
      {backWordsNow !== '' && <Text color={chrome.dim}>{`${' '.repeat(GAP)}${backWordsNow}`}</Text>}
    </Box>
  )
  const rule = (
    <Box flexDirection="row">
      {saying !== '' && <Text color={chrome.dim}>{`${' '.repeat(GAP)}${saying}${' '.repeat(GAP)}`}</Text>}
      <Client key="grip" module="./grip-client.tsx" props={{ width: ruleW, rows: win.area, ink: chrome.rule, lit: chrome.accent }} width={ruleW} height={RULE_ROWS} />
    </Box>
  )

  // Every row's hover reads its whole title and why it is where it is, `Line::tip`, once the pointer
  // has rested (src/tip-client.tsx), a tooltip at the row's left edge: under the row, or over it
  // where the band would clip it. A hover has no pointer to go by, so it cannot follow its column.
  // It shows by the row's hover group, out of the flow, and moves nothing.
  const head = boxRows + backRows
  const below = shorter ? 1 : 0
  const treeRows = head + RULE_ROWS + footerRows + win.shown + win.blank + below + (keeps ? RESERVED : 0)
  const seenTop = e.props.scroll.offset
  const seenBottom = treeRows > e.props.maxRows ? seenTop + e.props.scroll.bodyRows : treeRows
  const tipRoom = Math.max(1, Math.min(TIP_ROOM, width - INSET - TIP_EDGE_CELLS))
  const tip = (r: DisplayRow, i: number) => {
    const whole = tipLines(r, tipRoom, Number.MAX_SAFE_INTEGER)
    const { y, room } = tipPlace(head + RULE_ROWS + i, whole.length + TIP_EDGE_ROWS, seenTop, seenBottom)
    // In a band too short for its edge the words go bare.
    const edged = room > TIP_EDGE_ROWS
    const lines = tipLines(r, tipRoom, Math.max(1, edged ? room - TIP_EDGE_ROWS : room))
    return (
      <Box key={`hint-${r.id}`} position="absolute" top={y} left={INSET} display="none" hover={{ display: 'flex', scope: `row-${r.id}` }}>
        <Client key={`tip-${r.id}`} module="./tip-client.tsx" props={{ lines, ink: chrome.tipInk, fill: chrome.tip, edge: edged ? chrome.edge : null }} />
      </Box>
    )
  }

  return (
    <Box flexDirection="column">
      {box}
      {backRows > 0 && back}
      <Box flexDirection="column">
        {rule}
        {onRow.map(row)}
        {model.footer ? <Text color={chrome.dim} wrap="truncate">{`${' '.repeat(INSET)}${cutExact(model.footer, cols)}`}</Text> : undefined}
        {win.blank > 0 && <Box height={win.blank} />}
        {win.own && (
          <Box position="absolute" top={RULE_ROWS} left={width + BAR_GAP}>
            <Client
              key="bar"
              module="./bar-client.tsx"
              props={{ rows: win.shown, thumb, at: thumbAt, ink: chrome.dim, track: chrome.rule, lit: chrome.accent }}
              width={1}
              height={win.shown}
            />
          </Box>
        )}
      </Box>
      {shorter && back}
      {/* Claude Code's own rows under the prompt come over a worker's view and push the band up by
          as many: this room gives way to them, so the table stays on its screen lines. */}
      {keeps && <Box height={RESERVED} flexDirection="column">{under0 && back}</Box>}
      {onRow.map(tip)}
    </Box>
  )
}
