import type { Color, EngineInterface, RenderInput } from 'claude-code'

import type { DisplayRow, Model, Section } from '../types'

import { cells, cut, cutMiddle, padEnd, padStart } from './cells'
import { redrawSoon } from './poll'
import { readHidden, readPage, readSaid, readView, toggleFold } from './state'
import { GAUGE, INK, type Mark, markOf, SECTION_MARK, wantsEye } from './theme'
import { drawBox, goBack, openRow, viewedWords } from './worker'

// The band: a table in Claude's own gutter. The mark sits where Claude's bullets sit, the key where
// its text starts, and the eye reads down one column of marks to see how things stand. Nothing is
// boxed and nothing shouts: what is moving is bright, what waits or landed is dim, and a colour
// appears only where it means something (theme.ts). The one flourish is the gauge, a thin track
// with the estimate notched into it once a task runs past it.

const GAP = 2
/** Mark and the space after it. */
const GUTTER = 2
/** Most of the terminal the band takes unasked; the engine's own cap is `maxRows`. */
const SCREEN_SHARE = 0.4
/** The narrowest title cell a waiting row's reason shares. */
const INLINE_MIN = 28
const ALL = Number.MAX_SAFE_INTEGER

/** Widths in cells, 0 for a column not drawn. `tags`: titles keep their `BUG: area:`. */
export type Cols = { key: number; title: number; tags: boolean; gauge: number; elapsed: number; eta: number; now: number }

/** `12m`, `1h3m`, `1d3h12m`, `~8m`, `-~8m`, in minutes; undefined when it is not a span. */
export function minutes(words: string): number | undefined {
  const m = /^-?~?(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)m)?$/.exec(words.trim())
  if (!m || (m[1] ?? m[2] ?? m[3]) === undefined) return undefined
  return Number(m[1] ?? 0) * 1440 + Number(m[2] ?? 0) * 60 + Number(m[3] ?? 0)
}

/** How a running row's gauge reads, from the cells it draws; undefined with no estimate to draw. */
export function gauge(r: DisplayRow, width: number): { done: number; notch?: number } | undefined {
  if (r.own !== 'running' || width <= 0) return undefined
  const ran = minutes(r.elapsed)
  const rest = minutes(r.left)
  if (ran === undefined || rest === undefined) return undefined
  const inside = (n: number) => Math.min(width - 1, Math.max(1, n))
  if (r.over) return { done: width, notch: inside(Math.round((Math.max(0, ran - rest) / Math.max(1, ran)) * width)) }
  return { done: inside(Math.round((ran / Math.max(1, ran + rest)) * width)) }
}

/** The NOW cell. `Done` says nothing the mark does not: a landing only speaks when it is news. */
const nowWords = (r: DisplayRow) => {
  const what = r.own === 'done' && r.what.toLowerCase() === 'done' ? '' : r.what
  if (!r.oom) return what
  return what ? `OOM  ${what}` : 'OOM'
}

/** The ETA cell. With no clock column a Done row says how long it took: the measured span outranks the delta. */
export const etaCell = (r: DisplayRow, hasClock: boolean) => (!hasClock && r.own === 'done' && r.elapsed ? r.elapsed : r.left)

/** `BUG: auth(#12): the race` into what a table can lose and what it cannot. */
export function splitTitle(title: string): { tag: string; body: string } {
  const m = /^((?:[A-Z]{2,}: +)?(?:[\w./-]+(?:\(#\d+\))?: +)?)(.+)$/.exec(title)
  return m && m[1] ? { tag: m[1], body: m[2]! } : { tag: '', body: title }
}

/** A row's fold note for this width: what its folded rows are. */
const foldNote = (r: DisplayRow, width: number) => (r.fold ? (width < 100 ? r.fold.shortNote : r.fold.note) : '')

/**
 * The columns for these rows in `width` cells. Times keep their place at any width; the title gives
 * way first, then the gauge, then NOW, then the elapsed clock. Spare room stays at the right, so the
 * eye never has to cross a gap to get from a title to its times.
 */
export function columns(rows: DisplayRow[], width: number): Cols {
  const longest = (of: (r: DisplayRow) => string) => Math.max(0, ...rows.map(r => cells(of(r))))
  const key = Math.min(width < 70 ? 10 : 16, longest(r => r.key))
  const elapsed = width < 64 ? 0 : Math.max(3, longest(r => r.elapsed))
  const eta = Math.max(4, longest(r => etaCell(r, elapsed > 0)))
  const fixed = GUTTER + (key ? key + GAP : 0) + (elapsed ? elapsed + GAP : 0) + eta
  const room = width - fixed - GAP
  const hasGauge = rows.some(r => gauge(r, 8) !== undefined)
  const gaugeW = !hasGauge ? 0 : width >= 130 ? 14 : width >= 104 ? 8 : 0
  const afterGauge = room - (gaugeW ? gaugeW + GAP : 0)
  const wantNow = longest(nowWords)
  const now = wantNow > 0 && afterGauge >= 44 ? Math.min(wantNow, Math.max(16, Math.floor(afterGauge * 0.42))) : 0
  const titleRoom = Math.max(0, afterGauge - (now ? now + GAP : 0))
  // With no NOW column a waiting row's reason shares the title's cell, so the cell counts it.
  const beside = (r: DisplayRow) => {
    const reason = now === 0 && r.section === 'next' && r.what ? cells(r.what) + GAP : 0
    const note = foldNote(r, width)
    return cells(r.head) + reason + (note ? cells(note) + GAP : 0)
  }
  const want = (of: (r: DisplayRow) => string) => Math.max(0, ...rows.map(r => cells(of(r)) + beside(r)))
  // Types and areas stay on every row or on none: a column that drops them row by row reads as noise.
  const tags = want(r => r.title) <= titleRoom
  const title = Math.min(titleRoom, want(r => (tags ? r.title : splitTitle(r.title).body)))
  return { key, title, tags, gauge: gaugeW, elapsed, eta, now }
}

const wantsLook = (r: DisplayRow) => r.over || r.flag || r.oom || wantsEye(r) || markOf(r).color === INK.failed

/** Rows that want a look before the rest, a row's own agents after every row of its own. */
const byNeed = (rows: DisplayRow[]) => [
  ...rows.filter(r => r.depth === 0 && wantsLook(r)),
  ...rows.filter(r => r.depth === 0 && !wantsLook(r)),
  ...rows.filter(r => r.depth > 0),
]

/** `n` of these rows, in their own order, never a nested one without the row it hangs under. */
const keep = (rows: DisplayRow[], n: number) => {
  const kept = new Set(byNeed(rows).slice(0, n))
  let depthKept = 0
  return rows.filter(r => {
    if (!kept.has(r) || r.depth > depthKept) return false
    depthKept = r.depth + 1
    return true
  })
}

export type Plan = {
  running: DisplayRow[]
  next: DisplayRow[]
  done: DisplayRow[]
  /** Everything on the header's one line. */
  oneLine: boolean
  /** Room for a blank row between sections. */
  airy: boolean
  /** Room for the `+n` lines of Next up and Done. */
  summaries: boolean
  /** Room for the feed's footer. */
  footer: boolean
}

/** What fits in `budget` rows: every running row if it can, then Done and Next up give way. */
export function plan(model: Model, budget: number): Plan {
  const of = (s: Section) => model.rows.filter(r => r.section === s)
  const [running, next, done] = [of('running'), of('next'), of('done')]
  const tight = { next: [], done: [], airy: false, summaries: false, footer: false }
  if (budget <= 2) return { ...tight, running, oneLine: true }
  const rows = (shown: number, all: number) => Math.min(shown, all) + (all > shown ? 1 : 0)
  const tiers: [number, number][] = [[ALL, ALL], [6, 8], [6, 4], [4, 3], [3, 2], [2, 1], [1, 0], [0, 0]]
  for (const [n, d] of tiers) {
    const height = 1 + running.length + rows(n, next.length) + rows(d, done.length)
    if (height > budget) continue
    // Whatever room is over goes to the footer, then to a blank row between sections: the quietest line there is.
    const footer = model.footer !== undefined && model.footer !== '' && height + 1 <= budget
    const breaks = [running, next, done].filter(s => s.length > 0).length - 1
    return { running, next: keep(next, n), done: keep(done, d), oneLine: false, airy: height + (footer ? 1 : 0) + breaks <= budget, summaries: true, footer }
  }
  // More running than rows: those that want a look first, the rest in a +n line.
  return { ...tight, running: keep(running, budget - 2), oneLine: false }
}

/** One table row as text and what to draw it in; together exactly the row's cells. */
export type Seg = { text: string; color?: Color; dim?: boolean; bold?: boolean; toggle?: boolean }

export function rowSegs(r: DisplayRow, c: Cols, width = 200): Seg[] {
  const mark: Mark = markOf(r)
  const quiet = r.own !== 'running' || mark.dim === true
  const segs: Seg[] = [{ text: mark.glyph, color: mark.color, dim: mark.dim }, { text: ' ' }]
  const push = (text: string, style: Omit<Seg, 'text'> = {}) => segs.push({ dim: quiet, ...style, text })

  if (c.key) push(padEnd(cutMiddle(r.key, c.key), c.key + GAP), { bold: r.own === 'running' && !quiet })

  if (r.head) push(r.head, { dim: true, toggle: r.fold !== undefined })
  const left = c.title - cells(r.head)
  // With no NOW column a waiting row says why in its title's cell; the wait is what it is about.
  const inline = c.now === 0 && r.section === 'next' && c.title >= INLINE_MIN ? r.what : ''
  const inlineW = Math.min(cells(inline), Math.floor(left / 2))
  const note = foldNote(r, width)
  const noteW = note ? cells(note) + GAP : 0
  const titleW = left - (inlineW ? inlineW + GAP : 0) - noteW
  const whole = splitTitle(r.title)
  const tag = c.tags ? whole.tag : ''
  const body = cut(whole.body, titleW - cells(tag))
  if (tag) push(tag, { dim: true })
  push(body)
  if (note) push(`${' '.repeat(GAP)}${note}`, { dim: true })
  push(' '.repeat(Math.max(0, titleW - cells(tag) - cells(body))))
  if (inlineW) push(`${' '.repeat(GAP)}${cut(inline, inlineW)}`, { dim: true })
  push(' '.repeat(GAP))

  if (c.gauge) {
    const g = gauge(r, c.gauge)
    if (!g) push(' '.repeat(c.gauge))
    else {
      // Held, its gauge holds where it stopped, without its colour.
      const ink = (color: Color) => (quiet ? { dim: true } : { color, dim: false })
      if (g.notch !== undefined) {
        push(GAUGE.full.repeat(g.notch), ink(INK.moving))
        push(GAUGE.notch, ink(INK.behind))
        push(GAUGE.full.repeat(c.gauge - g.notch - 1), ink(INK.behind))
      } else {
        push(GAUGE.full.repeat(g.done), ink(INK.moving))
        push(GAUGE.empty.repeat(c.gauge - g.done), { dim: true })
      }
    }
    push(' '.repeat(GAP))
  }

  if (c.elapsed) push(padStart(r.elapsed, c.elapsed) + ' '.repeat(GAP), { dim: true })
  const eta = padEnd(etaCell(r, c.elapsed > 0), c.eta)
  if (r.over && !quiet) push(eta, { color: INK.behind, dim: false })
  else push(eta, { dim: quiet || r.noEta })

  if (c.now) {
    const words = nowWords(r)
    const ink = wantsEye(r) ? INK.eye : mark.color === INK.failed ? INK.failed : mark.glyph === '‖' && mark.color ? mark.color : undefined
    push(`${' '.repeat(GAP)}${cut(words, c.now)}`, ink ? { color: ink, dim: false } : { dim: true })
  }
  // A row ends on its last word, so a narrow terminal can never wrap on padding.
  while (segs.length > 0 && segs[segs.length - 1]!.text.trim() === '') segs.pop()
  const last = segs[segs.length - 1]
  if (last) last.text = last.text.trimEnd()
  return segs
}

/** `+1  lint, bench`: the rows a section could not show, by the keys that say which. */
export function moreWords(hidden: DisplayRow[], shownAny: boolean, word: string, width: number): string {
  const count = shownAny ? `+${hidden.length}` : `${hidden.length} ${word}`
  const names = hidden.filter(r => r.depth === 0).map(r => r.key || cut(r.title, 16)).join(', ')
  return cut(`${count}  ${names}`.trimEnd(), width - GUTTER)
}

/**
 * The header's counts in `room` cells, each thing that wants a look beside them in its ink: in
 * words, or by the marks the rows use when words do not fit.
 */
export function counts(model: Model, room: number): { text: string; color?: Color }[] {
  const behind = model.rows.filter(r => r.own === 'running' && r.over).length
  const eye = model.rows.filter(wantsEye).length
  const build = (running: string, over: string, next: string, done: string, review: string) => {
    const parts: { text: string; color?: Color }[] = []
    if (model.running) parts.push({ text: running })
    if (behind) parts.push({ text: over, color: INK.behind })
    if (model.next) parts.push({ text: next })
    if (model.done) parts.push({ text: done })
    if (eye) parts.push({ text: review, color: INK.eye })
    return parts
  }
  const words = build(`${model.running} running`, `${behind} over`, `${model.next} next`, `${model.done} done`, `${eye} to review`)
  if (words.reduce((n, p) => n + cells(p.text) + GAP, 0) <= room) return words
  const mark = SECTION_MARK
  return build(`${mark.running.glyph} ${model.running}`, `${mark.running.glyph} ${behind}`, `${mark.next.glyph} ${model.next}`, `${mark.done.glyph} ${model.done}`, `◆ ${eye}`)
}

/** The band's tree, or undefined with nothing to show. */
export async function drawBand($: EngineInterface, e: RenderInput<'AbovePrompt'>) {
  const model = await readView($)
  const words = await readSaid($)
  const page = await readPage($)
  const box = await drawBox($, e)
  if (e.props.hasSurvey || (await readHidden($)) || model.rows.length === 0) return undefined
  const { Box, Text, Button } = $.ui.resolve(e)
  const width = Math.max(20, e.props.bodyColumns)
  const screen = e.viewport?.rows
  const budget = Math.min(e.props.maxRows, screen ? Math.max(3, Math.floor(screen * SCREEN_SHARE)) : e.props.maxRows)
  // The box and the viewed line take rows of their own before the table.
  const shown = plan(model, budget - (box ? 3 : 0) - (e.props.view?.agentId ? 1 : 0))

  const fold = async (id: string) => {
    await toggleFold($, id)
    await redrawSoon()
  }

  // The name lights while a turn runs. Nothing moves or resizes for it: the band keeps its shape
  // across turns, so a row stays where the eye left it and where the pointer is about to press.
  const name = e.props.isWorking ? <Text color="claude" bold>Manager</Text> : <Text bold>Manager</Text>
  const backWords = width < 80 ? 'back' : 'back to the manager'
  const backButton = (e.props.view?.agentId || page) && (
    <Button key="back" hotkey="0" plain onPress={() => goBack($, e.props.view)}>
      {backWords}
    </Button>
  )
  const beside = cells('Manager') + GAP + (backButton ? cells(`0: ${backWords}`) + GAP : 0)
  const countsWidth = (parts: { text: string }[]) => parts.reduce((n, p) => n + cells(p.text) + GAP, 0)
  const tallied = (parts: { text: string; color?: Color }[]) =>
    parts.map(p => <Text color={p.color} dimColor={!p.color}>{p.text}</Text>)

  if (shown.oneLine) {
    const tally = counts(model, 0)
    let room = width - beside - countsWidth(tally)
    const chip = (r: DisplayRow) => `${r.key || cut(r.title, 16)} ${r.left}`.trimEnd()
    const named = byNeed(shown.running).filter(r => {
      room -= cells(chip(r)) + GAP
      return room >= 0
    })
    return (
      <Box flexDirection="row" gap={GAP}>
        {name}
        {tallied(tally)}
        {named.map(r => {
          const label = <Text color={r.over ? INK.behind : undefined} dimColor={!r.over}>{chip(r)}</Text>
          return (
            <Button key={`row-${r.id}`} plain onPress={() => openRow($, r, e.props.view, e.requestId)}>
              {label}
            </Button>
          )
        })}
        {backButton}
      </Box>
    )
  }

  const tally = counts(model, width - beside)
  const spare = width - beside - countsWidth(tally)
  const saying = words && spare >= 12 ? <Text dimColor>{cut(words, Math.min(50, spare))}</Text> : undefined
  const c = columns([...shown.running, ...shown.next, ...shown.done], width)

  const row = (r: DisplayRow) => {
    const segs = rowSegs(r, c, width)
    const drawn = (list: Seg[]) => list.map(s => <Text color={s.color} dimColor={s.dim} bold={s.bold}>{s.text}</Text>)
    const pressed = (list: Seg[], key: string) => (
      <Button key={key} plain onPress={() => openRow($, r, e.props.view, e.requestId)}>
        {drawn(list)}
      </Button>
    )
    const toggleAt = segs.findIndex(s => s.toggle)
    const toggler = r.fold
    if (toggler && toggleAt >= 0) {
      // A row with agents under it: its toggle folds them, the rest of it opens its worker.
      return (
        <Box key={`box-${r.id}`} flexDirection="row">
          {pressed(segs.slice(0, toggleAt), `row-${r.id}`)}
          <Button key={`fold-${r.id}`} plain onPress={() => fold(toggler.id)}>
            <Text>{segs[toggleAt]!.text}</Text>
          </Button>
          {pressed(segs.slice(toggleAt + 1), `rest-${r.id}`)}
        </Box>
      )
    }
    return pressed(segs, `row-${r.id}`)
  }
  const more = (section: Section, drawn: DisplayRow[], word: string) => {
    const all = model.rows.filter(r => r.section === section)
    const hidden = all.filter(r => !drawn.includes(r))
    if (hidden.length === 0) return undefined
    const mark: Mark = drawn.length === 0 ? SECTION_MARK[section] : { glyph: ' ' }
    return (
      <Text wrap="truncate">
        <Text color={mark.color} dimColor={mark.dim}>{`${mark.glyph} `}</Text>
        <Text dimColor>{moreWords(hidden, drawn.length > 0, word, width)}</Text>
      </Text>
    )
  }
  const section = (s: Section, drawn: DisplayRow[], word: string, first: boolean) => {
    const lines = [...drawn.map(row), s === 'running' || shown.summaries ? more(s, drawn, word) : undefined].filter(Boolean)
    return lines.length > 0 && shown.airy && !first ? [<Text> </Text>, ...lines] : lines
  }

  return (
    <Box flexDirection="column">
      <Box flexDirection="row" gap={GAP}>
        {name}
        {tallied(tally)}
        {backButton}
        {saying}
      </Box>
      {box}
      {e.props.view?.agentId && <Text dimColor wrap="truncate">{viewedWords(model, e.props.view)}</Text>}
      {section('running', shown.running, 'running', true)}
      {section('next', shown.next, 'next', model.running === 0)}
      {section('done', shown.done, 'done', model.running === 0 && model.next === 0)}
      {shown.footer && <Text dimColor wrap="truncate">{`${' '.repeat(GUTTER)}${cut(model.footer ?? '', width - GUTTER)}`}</Text>}
    </Box>
  )
}
