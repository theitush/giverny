import type { AgentInfo, On } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'

import type { DisplayRow, Model, Section } from '../types'

import { columns, counts, etaWords, gauge, moreWords, plan, rowSegs, splitTitle } from './band'
import { cells, cut, cutMiddle } from './cells'
import { INK, markOf } from './theme'

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
    { key: 'old', stage: 'done', title: 'CLEANUP: gone', started: '2026-11-04T09:00:00Z', ended: '2026-11-04T09:20:00Z', eta_s: 600, landing: 'Done' },
  ],
})

function engine(on: On, feed: string | undefined, agents: AgentInfo[]) {
  mock.clock(on, { now: NOW })
  mock.env(on, { GIVERNY_FEED_DIR: '/feeds' })
  on('command.register', () => ({ value: { command: 'manager' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: ME }))
  on('fs.stat', () => {
    if (feed === undefined) throw new Error('missing')
    return { value: { kind: 'file', size: 1, mtimeMs: 1, isLink: false } }
  })
  on('fs.list', () => ({ value: [] }))
  on('fs.read', () => ({ value: feed ?? '' }))
  on('agent.list', () => ({ value: agents }))
}

test('the band draws a feed, a row with a worker being the way to it', async ($, on) => {
  engine(on, FEED, [{ id: 'w1', description: 'build: fix it', type: 'general-purpose', status: 'running' }])
  on('ui.view', () => ({ value: {} }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /^1 running$/ })) !== undefined).toBe(true)
  expect((await ui.find({ key: 'row-build' })) !== undefined).toBe(true)
  await ui.press({ key: 'row-build' })
  expect((await ui.find({ type: 'Text', text: /viewing: BUG: the build/ })) !== undefined).toBe(true)
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

const row = (over: Partial<DisplayRow> & { key: string; section: Section }): DisplayRow => ({
  id: over.key,
  title: `BUG: area(#1): ${over.key} the thing itself`,
  elapsed: '',
  left: '',
  over: false,
  what: '',
  ...over,
})
const model = (rows: DisplayRow[]): Model => ({
  rows,
  running: rows.filter(r => r.section === 'running').length,
  next: rows.filter(r => r.section === 'next').length,
  done: rows.filter(r => r.section === 'done').length,
})
const busy = (running: number, next: number, done: number) =>
  model([
    ...Array.from({ length: running }, (_, i) =>
      row({ key: `r${i}`, section: 'running', elapsed: '14m', left: i === 1 ? 'over by 15m' : '~16m left', over: i === 1, what: 'building the test' }),
    ),
    ...Array.from({ length: next }, (_, i) => row({ key: `n${i}`, section: 'next', left: '~25m', what: i === 0 ? 'after r0' : '' })),
    ...Array.from({ length: done }, (_, i) => row({ key: `d${i}`, section: 'done', elapsed: '48m', left: 'est 50m', what: i === done - 1 ? 'Review — ita' : 'Done' })),
  ])
const text = (r: DisplayRow, c: ReturnType<typeof columns>) => rowSegs(r, c).map(s => s.text).join('')

test('at every width a row fits, and its times stay in one column', async () => {
  const rows = busy(3, 3, 3).rows
  for (const width of [200, 170, 130, 104, 100, 80, 64, 60, 44, 30]) {
    const c = columns(rows, width)
    const lines = rows.map(r => text(r, c))
    for (const l of lines) expect(cells(l) <= width).toBe(true)
    const at = lines.slice(0, 3).map(l => l.indexOf('~16m') === -1 ? l.indexOf('+15m') : l.indexOf('~16m'))
    expect(new Set(at).size).toBe(1)
  }
})

test('narrowing loses the gauge, then NOW, then the clock; the key and the ETA last', async () => {
  const rows = busy(2, 1, 1).rows
  const wide = columns(rows, 170)
  expect(wide.gauge > 0 && wide.now > 0 && wide.elapsed > 0).toBe(true)
  expect(columns(rows, 100).gauge).toBe(0)
  expect(columns(rows, 100).now > 0).toBe(true)
  const narrow = columns(rows, 50)
  expect([narrow.gauge, narrow.now, narrow.elapsed]).toEqual([0, 0, 0])
  expect(narrow.key > 0 && narrow.eta > 0).toBe(true)
})

test('a title drops its type and area on every row or on none', async () => {
  const rows = [
    row({ key: 'a', section: 'running', title: 'BUG: auth(#12): a long title that will not fit in a narrow table at all', left: '~1m left', elapsed: '1m' }),
    row({ key: 'b', section: 'next', title: 'RUN: short', left: '~1m' }),
  ]
  expect(splitTitle('BUG: auth(#12): the race')).toEqual({ head: 'BUG: auth(#12): ', body: 'the race' })
  expect(splitTitle('plain: words')).toEqual({ head: 'plain: ', body: 'words' })
  expect(splitTitle('no head here').head).toBe('')
  const wide = columns(rows, 170)
  expect(wide.heads).toBe(true)
  expect(text(rows[1]!, wide).includes('RUN: short')).toBe(true)
  const tight = columns(rows, 60)
  expect(tight.heads).toBe(false)
  expect(text(rows[1]!, tight).includes('RUN:')).toBe(false)
  expect(text(rows[0]!, tight).includes('a long title')).toBe(true)
})

test('with no NOW column a waiting row still says why, and a Done row what it took', async () => {
  const rows = busy(1, 1, 1).rows
  const c = columns(rows, 50)
  expect(c.now).toBe(0)
  expect(text(rows[1]!, c).includes('after r0')).toBe(true)
  const clockless = c
  expect(clockless.elapsed).toBe(0)
  expect(text(rows[2]!, clockless).includes('48m')).toBe(true)
  expect(text(rows[2]!, clockless).includes('est')).toBe(false)
})

test('the plan gives up Done and Next up before any running row, keeping what wants a look', async () => {
  const m = busy(3, 7, 5)
  const roomy = plan(m, 20)
  expect([roomy.running.length, roomy.next.length, roomy.done.length]).toEqual([3, 6, 4])
  expect(roomy.airy).toBe(true)
  expect(plan(m, 17).airy).toBe(false)
  // The newest four are d0..d3; d4 is the only one to review, and it stays.
  expect(roomy.done.map(r => r.key)).toEqual(['d0', 'd1', 'd2', 'd4'])
  const short = plan(m, 7)
  expect(short.running.length).toBe(3)
  expect(short.next.length + short.done.length).toBe(1)
  const least = plan(m, 6)
  expect(least.running.length).toBe(3)
  expect(least.next.length + least.done.length).toBe(0)
})

test('more running than rows keeps the one past its estimate, in order, with a +n line in the budget', async () => {
  const p = plan(busy(9, 2, 2), 4)
  expect(p.running.map(r => r.key)).toEqual(['r0', 'r1'])
  expect(p.summaries).toBe(false)
  expect(plan(busy(9, 2, 2), 3).running.map(r => r.key)).toEqual(['r1'])
})

test('two rows or fewer is the one-line form', async () => {
  expect(plan(busy(3, 2, 2), 2).oneLine).toBe(true)
  expect(plan(busy(3, 2, 2), 3).oneLine).toBe(false)
})

test('the counts say in words what fits, by marks what does not, and call out what wants a look', async () => {
  const m = busy(3, 1, 2)
  expect(counts(m, 100).map(p => p.text)).toEqual(['3 running', '1 over', '1 next', '2 done', '1 to review'])
  expect(counts(m, 100)[1]!.color).toBe(INK.behind)
  expect(counts(m, 10).map(p => p.text)).toEqual(['● 3', '● 1', '○ 1', '✓ 2', '◆ 1'])
  expect(counts(busy(1, 0, 0), 100).map(p => p.text)).toEqual(['1 running'])
})

test('a mark says what a row is in one cell', async () => {
  const mark = (over: Partial<DisplayRow> & { section: Section }) => markOf(row({ key: 'k', ...over }))
  expect(mark({ section: 'running', left: '~1m left' }).color).toBe(INK.moving)
  expect(mark({ section: 'running', left: 'over by 3m', over: true }).color).toBe(INK.behind)
  expect(mark({ section: 'running', left: 'paused' }).glyph).toBe('‖')
  expect(mark({ section: 'running', what: 'asleep · wrangler deploy (30m) · wakes when it ends' }).glyph).toBe('◌')
  expect(mark({ section: 'running', what: 'killed 3m ago' }).color).toBe(INK.failed)
  expect(mark({ section: 'next' }).glyph).toBe('○')
  expect(mark({ section: 'next', what: 'after x' }).glyph).toBe('◌')
  expect(mark({ section: 'next', what: 'held: a quiet machine' }).glyph).toBe('‖')
  expect(mark({ section: 'done', what: 'Done' }).glyph).toBe('✓')
  expect(mark({ section: 'done', what: 'Review — ita' }).color).toBe(INK.eye)
  expect(mark({ section: 'done', what: 'Blocked' }).color).toBe(INK.failed)
})

test('the gauge fills with the clock and notches the estimate once past it', async () => {
  const r = (elapsed: string, left: string) => row({ key: 'k', section: 'running', elapsed, left })
  expect(gauge(r('15m', '~15m left'), 10)).toEqual({ done: 5 })
  expect(gauge(r('<1m', '~30m left'), 10)).toEqual({ done: 1 })
  expect(gauge(r('40m', 'over by 20m'), 10)).toEqual({ done: 10, notch: 5 })
  expect(gauge(r('1h15', 'over by 1h'), 10)?.notch).toBe(2)
  expect(gauge(r('5m', 'no ETA'), 10)).toBe(undefined)
  expect(gauge(row({ key: 'k', section: 'next', left: '~5m' }), 10)).toBe(undefined)
  expect(etaWords('~8m left')).toBe('~8m')
  expect(etaWords('over by 3m')).toBe('+3m')
})

test('truncation keeps what tells rows apart', async () => {
  expect(cut('abcdef', 4)).toBe('abc…')
  expect(cut('宽字符宽字符', 5)).toBe('宽字…')
  expect(cells('宽x')).toBe(3)
  expect(cutMiddle('task-522-manager-mod', 12)).toBe('task-5…r-mod'.length === 12 ? 'task-5…r-mod' : '')
  expect(moreWords(busy(0, 2, 0).rows, true, 'next', 80)).toBe('+2  n0, n1')
  expect(moreWords(busy(0, 2, 0).rows, false, 'next', 80)).toBe('2 next  n0, n1')
})
