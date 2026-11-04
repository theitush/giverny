import type { AgentInfo, On } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'

import type { DisplayRow, Model, Section } from '../types'

import { ALL_ON, bandWindow, cutRuns, fitTitle, foldX, layout, rowRuns, rowText, switchesOf, tipLines, usageCell } from './band'
import { cells, cut, cutExact, cutMiddle } from './cells'
import { DARK, LIGHT, STAGE, STAGE_INK, tint, VIEWED_TINT, chromeOf } from './theme'
import { backWords } from './worker'

const BAND = {
  plugin: 'manager',
  component: 'AbovePrompt',
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 150, scroll: { offset: 0, bodyRows: 20 }, view: {} },
} as const

const ME = '11111111-1111-4111-8111-111111111111'
const NOW = Date.parse('2026-11-04T10:00:00Z')
const FEED = JSON.stringify({
  rows: [
    { key: 'build', stage: 'running', title: 'BUG: the build', started: '2026-11-04T09:50:00Z', eta_s: 1800, note: 'linking' },
    { key: 'next', stage: 'planned', title: 'FEATURE: after it', eta_s: 600, after: 'build' },
    { key: 'old', stage: 'done', title: 'CLEANUP: gone', started: '2026-11-04T09:00:00Z', ended: '2026-11-04T09:20:00Z', eta_s: 600, landing: 'Done', usage: { runs: 1 } },
  ],
})

function engine(on: On, feed: string | undefined, agents: AgentInfo[], marked = true) {
  mock.clock(on, { now: NOW })
  mock.env(on, { GIVERNY_FEED_DIR: '/feeds' })
  on('command.register', () => ({ value: { command: 'manager' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: ME }))
  on('fs.stat', (_, e) => {
    if (feed === undefined || (!marked && e.path.includes('/managers/'))) throw new Error('missing')
    return { value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false } }
  })
  on('fs.list', () => ({ value: [] }))
  on('fs.read', () => ({ value: feed ?? '' }))
  on('agent.list', () => ({ value: agents }))
}

test('the band draws a feed, a row with a worker being the way to it', async ($, on) => {
  engine(on, FEED, [{ id: 'w1', description: 'build: fix it', type: 'general-purpose', status: 'running' }])
  const asked: unknown[] = []
  on('ui.view', (_, e) => {
    asked.push(e)
    return { value: {} }
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /Running/ })) !== undefined).toBe(true)
  expect((await ui.find({ key: 'row-build' })) !== undefined).toBe(true)
  await ui.press({ key: 'row-build' })
  expect(asked).toEqual([{ agentId: 'w1' }])
  await ui.unmount()
})

test('with nothing to show the engine draws the band', async ($, on) => {
  engine(on, undefined, [])
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)
    return <Text>engine</Text>
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text' }))?.text).toBe('engine')
  await ui.unmount()
})

test('a survey holds the band: it yields', async ($, on) => {
  engine(on, FEED, [])
  on('ui.render', ($, e) => {
    const { Text } = $.ui.resolve(e)
    return <Text>engine</Text>
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const survey = await $.ui.mount({ ...BAND, props: { ...BAND.props, hasSurvey: true }, surface: 'terminal' })
  expect((await survey.find({ type: 'Text' }))?.text).toBe('engine')
  await survey.unmount()
})

const row = (over: Partial<DisplayRow> & { key: string; section: Section }): DisplayRow => ({
  id: over.key,
  own: over.section,
  title: `BUG: area(#1): ${over.key} the thing itself`,
  head: '',
  depth: 0,
  elapsed: '',
  left: '',
  over: false,
  noEta: false,
  what: '',
  flag: false,
  tokens: '',
  oom: false,
  why: [],
  facts: [],
  lease: '',
  finished: false,
  asleep: false,
  ...over,
})
const COLS = 166
const running = row({ key: 'mgr-copy', section: 'running', title: 'FEATURE: the band', elapsed: '1:10', left: '~34m', what: 'Bash: capture', tokens: '106.8k', use: { cpuPct: 0, memMb: 102, cpuLoad: 'light', memLoad: 'light' } })
const done = row({ key: 'mgr-cols', section: 'done', title: 'FEATURE: columns', elapsed: '28:37', left: '(-2m)', what: 'Review — ita', tokens: '258.7k', use: { memMb: 0, cpuLoad: 'light', memLoad: 'light' } })
const next = row({ key: 'bench', section: 'next', title: 'RESEARCH: how slow', left: '~40m', what: 'queued for 8G behind a-long-task-name' })
const table = [running, next, done]
const text = (r: DisplayRow, rows = table, cols = COLS, on = ALL_ON) => rowText(rowRuns(r, layout(rows, cols, on), DARK, cols))

test('a row reads as the panel reads it: the stage word, then each column in its place', async () => {
  const line = text(running)
  expect(line.startsWith('Running  mgr-copy FEATURE: the band')).toBe(true)
  expect(text(next).startsWith('NextUp   bench    RESEARCH')).toBe(true)
  expect(text(done).startsWith('Done     mgr-cols FEATURE')).toBe(true)
  // ELAPSED and ETA end on their columns' last cells, right-aligned: 8 and 10 wide, a gap between.
  const l = layout(table, COLS)
  expect(l.xEtaEnd - l.xElEnd).toBe(10 + 2)
  expect(line.indexOf('1:10') + 4).toBe(l.xElEnd)
  expect(text(done).indexOf('28:37') + 5).toBe(l.xElEnd)
  expect(line.indexOf('~34m') + 4).toBe(l.xEtaEnd)
  expect(text(done).indexOf('(-2m)') + 5).toBe(l.xEtaEnd)
  expect(line.indexOf('Bash: capture')).toBe(l.xNow)
  expect(line.indexOf('106.8k') + 6).toBe(l.xTokEnd)
  // The use column ends on the table's last cell, a row's figures in the panel's slots.
  expect(line.endsWith('    0% CPU  ·  0.1G')).toBe(true)
  expect(line.length).toBe(COLS)
  expect(text(done).endsWith('0.0G')).toBe(true)
  expect(text(done).length).toBe(COLS)
})

test('the whole row is its stage’s colour, in the panel’s own codes', async () => {
  expect(STAGE).toEqual({ running: '#0087d7', next: '#5f87af', done: '#008700' })
  expect(STAGE_INK).toEqual({ running: 'ansi256(32)', next: 'ansi256(67)', done: 'ansi256(28)' })
  for (const r of table) expect(rowRuns(r, layout(table, COLS), DARK, COLS).map(s => s.ink)).toEqual([STAGE_INK[r.own]])
  // Past its estimate is not a colour of its own; a Review landing is not either.
  const over = { ...running, over: true, left: '-~3m' }
  expect(rowRuns(over, layout(table, COLS), DARK, COLS).map(s => s.ink)).toEqual([STAGE_INK.running])
})

test('only a missing ETA, a flag, an OOM and a figure’s weight break a row’s colour', async () => {
  const inks = (r: DisplayRow, c = DARK) => rowRuns(r, layout([r], COLS), c, COLS)
  const of = (r: DisplayRow, word: string, c = DARK) => inks(r, c).find(s => s.text.includes(word))?.ink
  expect(of({ ...running, noEta: true, left: 'no ETA' }, 'no ETA')).toBe(DARK.dim)
  expect(of({ ...running, flag: true, what: 'stopped: no network' }, 'stopped:')).toBe(DARK.amber)
  expect(of({ ...running, flag: true, what: 'stopped: no network' }, 'stopped:', LIGHT)).toBe(LIGHT.amber)
  const oom = { ...running, oom: true }
  expect(of(oom, 'OOM')).toBe(DARK.amber)
  expect(usageCell(oom).startsWith('OOM ')).toBe(true)
  const heavy = { ...running, use: { cpuPct: 62, memMb: 22000, cpuLoad: 'heavy', memLoad: 'busy' } as const }
  expect(of(heavy, '62% CPU')).toBe(DARK.poppy)
  expect(of(heavy, '21G')).toBe(DARK.amber)
  expect(of(heavy, 'mgr-copy')).toBe(STAGE_INK.running)
})

test('a use cell is all one width, nothing measured reading as zero, and Next up has none', async () => {
  expect(usageCell(running)).toBe('      0% CPU  ·  0.1G')
  expect(usageCell({ ...running, use: undefined })).toBe('      0% CPU  ·  0.0G')
  expect(usageCell(done)).toBe('                 0.0G')
  expect(usageCell({ ...done, oom: true })).toBe('OOM              0.0G')
  expect(usageCell(next)).toBe('')
  expect(layout([next], COLS).usew).toBe(0)
})

test('a row with no tokens lends NOW the TOKENS column', async () => {
  expect(text(next).includes('queued for 8G behind a-long-task')).toBe(true)
  expect(text(done).includes('Review — ita')).toBe(true)
  const long = { ...done, what: 'a landing longer than the column can hold, by far' }
  expect(text(long).includes('a landing longer than the co…')).toBe(false)
  expect(text(long).includes('a landing longer than the c…')).toBe(true)
})

test('at any width the columns keep their widths and the table runs off the right', async () => {
  for (const cols of [120, 80, 60, 40]) {
    const l = layout(table, cols)
    expect(l.xEtaEnd - l.xElEnd).toBe(12)
    for (const r of table) expect(cells(text(r, table, cols)) <= cols).toBe(true)
  }
  const tight = text(running, table, 60)
  expect(tight.startsWith('Running  mgr-copy FEATURE: th…')).toBe(true)
  expect(tight.includes('~34m')).toBe(true)
  expect(tight.includes('106.8k')).toBe(false)
})

test('a title gives way first, then a folded row’s note shortens, then it drops', async () => {
  const fold = { id: 'p', folded: true, under: 6, running: 2, note: '+6, 2 running', shortNote: '+6' }
  const parent = row({ key: 'p', section: 'running', title: 'Work on the thing', head: '▸ ', fold })
  expect(fitTitle(parent, 40)).toBe('▸ Work on the thing  +6, 2 running')
  expect(fitTitle(parent, 24)).toBe('▸ Work o…  +6, 2 running')
  expect(fitTitle(parent, 14)).toBe('▸ Work on…  +6')
  expect(fitTitle(parent, 9)).toBe('▸ Work o…')
  expect(fitTitle({ ...parent, fold: { ...fold, folded: false, note: '', shortNote: '' }, head: '▾ ' }, 40)).toBe('▾ Work on the thing')
})

test('a nested row hangs under its parent by its own stage, and the toggle has its own press', async () => {
  const fold = { id: 'p', folded: false, under: 1, running: 1, note: '', shortNote: '' }
  const parent = row({ key: 'p', section: 'running', title: 'Work', head: '▾ ', fold })
  const kid = row({ key: '', section: 'running', own: 'done', depth: 1, head: '   └ ', title: 'helper', what: 'done' })
  const rows = [parent, kid]
  expect(text(kid, rows).startsWith('Done          └ helper')).toBe(true)
  expect(rowRuns(kid, layout(rows, COLS), DARK, COLS)[0]!.ink).toBe(STAGE_INK.done)
  const l = layout(rows, COLS)
  const at = foldX(parent, l)!
  const [before, rest] = cutRuns(rowRuns(parent, l, DARK, COLS), at)
  const [glyph, after] = cutRuns(rest, 2)
  expect(glyph.map(s => s.text).join('')).toBe('▾ ')
  expect(before.map(s => s.text).join('')).toBe('Running  p ')
  expect(after.map(s => s.text).join('').startsWith('Work')).toBe(true)
})

test('a column switched off takes no width, the rest close up, and TASK takes what is left', async () => {
  const off = (name: keyof typeof ALL_ON) => text(running, table, COLS, { ...ALL_ON, [name]: false })
  expect(off('tokens').includes('106.8k')).toBe(false)
  expect(off('usage').includes('CPU')).toBe(false)
  expect(off('now').includes('Bash')).toBe(false)
  expect(off('elapsed').includes('1:10')).toBe(false)
  expect(off('eta').includes('~34m')).toBe(false)
  expect(off('id').startsWith('Running  FEATURE')).toBe(true)
  expect(off('title').includes('FEATURE')).toBe(false)
  expect(off('stage').startsWith('mgr-copy FEATURE')).toBe(true)
  const none = { stage: false, id: false, title: false, elapsed: false, eta: false, now: false, tokens: false, usage: false }
  expect(text(running, table, COLS, none)).toBe('')
})

test('a switch in the manifest reaches the band', { options: { column_title: false, column_elapsed: false } }, async ($, on) => {
  engine(on, FEED, [{ id: 'w1', description: 'build: fix it', type: 'general-purpose', status: 'running' }])
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /Running/ })) !== undefined).toBe(true)
  // The row has no title and no clock; only its hover reads the title, as the panel's.
  const line = await ui.find({ type: 'Text', text: /^Running +build/ })
  expect(line !== undefined && !/the build|\d:\d\d/.test(line.text ?? '')).toBe(true)
  await ui.unmount()
})

test('the manifest’s fields are the switches, a missing one on', async () => {
  expect(switchesOf({})).toEqual(ALL_ON)
  expect(switchesOf({ column_usage: false, column_id: true }).usage).toBe(false)
  expect(switchesOf({ column_usage: false, column_id: true }).id).toBe(true)
})

test('there is no gauge, no mark, no count line and no heading: the tree is a rule and the rows', async ($, on) => {
  engine(on, FEED, [])
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /[●○◌‖✓◆×━╋]|\d running|\d next|STAGE|Manager/ })).toBe(undefined)
  expect((await ui.find({ key: 'grip' })) !== undefined).toBe(true)
  await ui.unmount()
})

const viewed = { ...BAND, props: { ...BAND.props, view: { agentId: 'w1' } } }

test('while a worker is on view its row is lit, the way back has words, and the reserved room gives way', async ($, on) => {
  engine(on, FEED, [{ id: 'w1', description: 'build: fix it', type: 'general-purpose', status: 'running' }])
  const asked: unknown[] = []
  on('ui.view', (_, e) => {
    asked.push(e)
    return { value: {} }
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const rest = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await rest.find({ key: 'back' })).toBe(undefined)
  expect(await rest.find({ type: 'Text', text: /▌/ })).toBe(undefined)
  await rest.unmount()

  const ui = await $.ui.mount({ ...viewed, surface: 'terminal' })
  expect((await ui.find({ key: 'back' })) !== undefined).toBe(true)
  expect((await ui.find({ type: 'Text', text: /↺ back to manager/ })) !== undefined).toBe(true)
  expect((await ui.find({ type: 'Text', text: /Esc at the prompt stops this worker/ })) !== undefined).toBe(true)
  expect((await ui.find({ type: 'Text', text: /▌/ })) !== undefined).toBe(true)
  const lit = await ui.find({ type: 'Text', text: /the build/ })
  expect((lit?.props as { backgroundColor?: string }).backgroundColor).toBe(tint(STAGE.running, DARK.bg, VIEWED_TINT))
  await ui.press({ key: 'back' })
  expect(asked).toEqual([{}])
  await ui.press({ key: 'row-build' })
  expect(asked).toEqual([{}, {}])
  await ui.unmount()
})

test('the words beside the back button fit their room, and only warn of Esc over a running worker', async () => {
  const m = { rows: [{ ...running, workerId: 'w1' }, { ...done, workerId: 'w2' }], running: 1, next: 0, done: 1, nested: false }
  const over = (id: string, room: number) => backWords(m, { agentId: id }, undefined, room)
  expect(over('w1', 200).includes('Esc at the prompt stops this worker')).toBe(true)
  expect(over('w1', 60)).toBe('or press 0, or click its row again  ·  Esc stops this worker')
  expect(over('w1', 12)).toBe('Esc stops it')
  expect(over('w2', 200).includes('Esc')).toBe(false)
  expect(over('w2', 200)).toBe('or press 0 at the empty prompt, or click its row again')
  expect(over('w1', 3)).toBe('')
})

test('the dark and light chrome both read', async () => {
  expect(chromeOf('dark')).toBe(DARK)
  expect(chromeOf('light-daltonized')).toBe(LIGHT)
  expect(chromeOf(undefined)).toBe(DARK)
  expect(tint('#0087d7', '#000000', 0.5)).toBe('#00446c')
  expect(tint('#000000', '#ffffff', 0)).toBe('#ffffff')
})

test('truncation', async () => {
  expect(cut('abcdef', 4)).toBe('abc…')
  expect(cutExact('ab cdef', 4)).toBe('ab …')
  expect(cut('宽字符宽字符', 5)).toBe('宽字…')
  expect(cells('宽x')).toBe(3)
  expect(cutMiddle('task-522-manager-mod', 12)).toBe('task-5…r-mod')
})

const PLANNED = JSON.stringify({ writer: 'giverny/manage', rows: [{ key: 'later', stage: 'planned', title: 'FEATURE: later', eta_s: 600 }] })
const WORKED = JSON.stringify({
  writer: 'giverny/manage',
  rows: [
    { key: 'w', stage: 'running', title: 'BUG: worked', started: '2026-11-04T09:50:00Z', eta_s: 600, agent_id: 'w1' },
    { key: 'stray', stage: 'running', title: 'BUG: a note to itself', started: '2026-11-04T09:50:00Z', eta_s: 600, usage: { runs: 1 } },
  ],
})

/** No mark on disk, and Giverny's plugin on or off. */
function unmarked(on: On, feed: string, agents: AgentInfo[], enabledPlugins: Record<string, boolean>) {
  engine(on, feed, agents, false)
  on('settings.read', () => ({ value: { enabledPlugins } }) as never)
}

test('with no Giverny hook a planned row makes a manager, and its Next up draws', async ($, on) => {
  unmarked(on, PLANNED, [], {})
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /NextUp/ })) !== undefined).toBe(true)
})

test('with Giverny’s hook on only its mark makes a manager, whatever was planned', async ($, on) => {
  unmarked(on, PLANNED, [], { 'giverny@giverny': true })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await expect($.ui.mount({ ...BAND, surface: 'terminal' })).rejects.toThrow(/no implementation for ui.render/)
})

test('a plain session with workers and no plan is no manager: a row with no worker stays off', async ($, on) => {
  unmarked(on, WORKED, [{ id: 'w1', description: 'w: worked', type: 'general-purpose', status: 'running' }], {})
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /Running/ })) !== undefined).toBe(true)
  expect(await ui.find({ type: 'Text', text: /a note to itself/ })).toBe(undefined)
})

const PANE_SITE = {
  plugin: 'manager',
  component: 'Pane',
  requestId: 'worker',
  surface: 'terminal',
  props: { title: 'w', isFocused: true, bodyColumns: 80, placement: 'dock', scroll: { offset: 0, bodyRows: 40 }, view: {} },
} as const

test('a worker Claude Code no longer lists opens a page, and a line typed there reaches the worker', async ($, on) => {
  engine(on, FEED, [{ id: 'w1', description: 'build: fix it', type: 'general-purpose', status: 'completed' }])
  on('ui.view', () => ({ value: { deny: 'no agent under that id' } }))
  const opened: unknown[] = []
  on('ui.open', (_, e) => {
    opened.push(e.id)
    return { value: { isPlaced: true } }
  })
  on('ui.close', () => ({ value: undefined }))
  on('session.messages', () => ({ value: [{ role: 'user', text: 'Fix the build', toolUses: [] }, { role: 'assistant', text: 'It builds.', toolUses: [] }] }))
  const sent: unknown[] = []
  on('session.send', (_, e) => {
    sent.push([e.to, e.text])
    return { isDelivered: true }
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'row-build' })
  expect(opened).toEqual(['worker'])
  const page = await $.ui.mount(PANE_SITE)
  expect((await page.find({ type: 'Markdown', text: /It builds/ })) !== undefined).toBe(true)
  await band.input({ key: 'say-0', text: 'stop after this file' })
  expect(sent).toEqual([['w1', 'stop after this file']])
  expect((await band.find({ type: 'Text', text: /Sent to this worker/ })) !== undefined).toBe(true)
  expect((await page.find({ type: 'Text', text: /stop after this file/ })) !== undefined).toBe(true)
  // A second press goes back; the box goes with the page, and a third opens it again.
  await band.press({ key: 'row-build' })
  expect(await band.find({ key: 'say-1' })).toBe(undefined)
  await band.press({ key: 'row-build' })
  expect(opened).toEqual(['worker', 'worker'])
  await page.unmount()
  await band.unmount()
})

test('a band never set follows its rows; set, it holds its height, a window over them when shorter', () => {
  expect(bandWindow(9, 0, null, 15, 0)).toEqual({ top: 0, shown: 9, blank: 0, area: 9, own: false })
  // Taller than the site: the engine's to scroll, as it was.
  expect(bandWindow(30, 0, null, 15, 7)).toEqual({ top: 0, shown: 30, blank: 0, area: 15, own: false })
  expect(bandWindow(30, 0, 99, 15, 7)).toEqual({ top: 0, shown: 30, blank: 0, area: 15, own: false })
  // Taller than its rows: the room stays, under them.
  expect(bandWindow(3, 1, 8, 15, 0)).toEqual({ top: 0, shown: 3, blank: 4, area: 8, own: false })
  expect(bandWindow(9, 0, 4, 15, 2)).toEqual({ top: 2, shown: 4, blank: 0, area: 4, own: true })
  expect(bandWindow(9, 0, 4, 15, 20).top).toBe(5)
  // The footer is one of the rows it was set to.
  expect(bandWindow(9, 1, 4, 15, 0)).toEqual({ top: 0, shown: 3, blank: 0, area: 4, own: true })
})

const tipRow = { key: 'k', title: 'a title of twenty ch', why: ['why one', 'why two is longer than that'] } as unknown as DisplayRow

test('a hover says the title and its why, whole while the rows hold it, and cut only at the end of the last', () => {
  expect(tipLines(tipRow, 100, 1)).toEqual(['a title of twenty ch  ·  why one  ·  why two is longer than that'])
  expect(tipLines(tipRow, 40, 2)).toEqual(['a title of twenty ch  ·  why one', 'why two is longer than that'])
  expect(tipLines(tipRow, 40, 1)[0]).toBe(cut('a title of twenty ch  ·  why one  ·  why two is longer than that', 40))
  expect(tipLines({ ...tipRow, title: '', why: [] }, 40, 2)).toEqual(['k'])
  // One part longer than a line runs on to the next.
  expect(tipLines({ ...tipRow, title: 'x'.repeat(50), why: [] }, 40, 2)).toEqual(['x'.repeat(40), 'x'.repeat(10)])
})

const ROWS = JSON.stringify({ rows: Array.from({ length: 9 }, (_, i) => ({ key: `t${i}`, stage: 'planned', title: `title ${i}`, eta_s: 600 })) })
const rowsShown = async (ui: { find: (q: { type: 'Text'; text: RegExp }) => Promise<unknown> }) => {
  const shown: number[] = []
  for (let i = 0; i < 9; i++) if ((await ui.find({ type: 'Text', text: new RegExp(`t${i} title ${i}`) })) !== undefined) shown.push(i)
  return shown
}

test('dragging the rule sets the height kept across sessions; a press with no move leaves it following its rows', async ($, on) => {
  engine(on, ROWS, [])
  const kept: Record<string, unknown> = {}
  on('store.get', () => ({ value: undefined }))
  on('store.set', (_, e) => {
    kept[e.key] = e.value
    return { value: undefined }
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await rowsShown(ui)).length).toBe(9)
  await ui.pointer({ type: 'down', x: 3, y: 0, button: 'left', in: 'grip' })
  await ui.pointer({ type: 'up', x: 3, y: 0, button: 'left', in: 'grip' })
  expect(kept['band-rows']).toBe(undefined)
  await ui.pointer({ type: 'down', x: 3, y: 0, button: 'left', in: 'grip' })
  await ui.pointer({ type: 'move', x: 3, y: 5, button: 'left', in: 'grip' })
  expect(await rowsShown(ui)).toEqual([0, 1, 2, 3])
  // The rule is at the pointer by now, so its row against the rule's is none.
  await ui.pointer({ type: 'up', x: 3, y: 0, button: 'left', in: 'grip' })
  expect(kept['band-rows']).toBe(4)
  // Shorter than its rows: the bar is the mouse's way along them, and its end is the last row.
  await ui.pointer({ type: 'down', x: 0, y: 3, button: 'left', in: 'bar' })
  expect(await rowsShown(ui)).toEqual([5, 6, 7, 8])
  await ui.unmount()
})

test('a height kept from before is read at the start, and a smaller one holds at the fewest', async ($, on) => {
  engine(on, ROWS, [])
  mock.store(on, { 'band-rows': 1 })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await rowsShown(ui)).toEqual([0, 1])
  await ui.unmount()
})

test('the hover text waits for egui\'s half a second, and leaving the row starts it over', async ($, on) => {
  engine(on, ROWS, [])
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const tip = 'tip-t0'
  const showing = async () => (await ui.find({ type: 'Text', text: /title 0 +· /, in: tip })) !== undefined
  await ui.resize({ columns: 100, rows: 1, in: tip })
  await ui.advance(400)
  expect(await showing()).toBe(false)
  await ui.resize({ columns: 0, rows: 0, in: tip })
  await ui.advance(400)
  await ui.resize({ columns: 100, rows: 1, in: tip })
  await ui.advance(400)
  expect(await showing()).toBe(false)
  await ui.advance(100)
  expect(await showing()).toBe(true)
  await ui.resize({ columns: 0, rows: 0, in: tip })
  expect(await showing()).toBe(false)
  await ui.unmount()
})
