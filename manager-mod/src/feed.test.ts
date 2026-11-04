import type { On } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'

import { parse } from './feed'

const ME = '11111111-1111-4111-8111-111111111111'
const DIR = '/feeds'
const file = (mtimeMs: number, size: number) => ({ kind: 'file', size, mtimeMs, isLink: false }) as const

test('parse reads what the doc allows and drops what it cannot place', async () => {
  const f = parse(
    JSON.stringify({
      aliases: ['old', 7, ''],
      footer: { text: 'a total' },
      rows: [
        { key: 'a', stage: 'In Progress', eta_s: '2400', tokens: -1, started: '2026-09-23T10:00:00Z', ended: 1758621600000, title: ' t ', note: null },
        { agent_id: 'w1', stage: 'queued' },
        { key: 'no-stage' },
        { key: 'odd', stage: 'sideways' },
        { stage: 'done' },
        'not a row',
      ],
    }),
  )
  expect(f?.rows.map(r => [r.key, r.stage])).toEqual([
    ['a', 'running'],
    ['w1', 'planned'],
  ])
  const a = f?.rows[0]
  expect([a?.eta_s, a?.tokens, a?.title, a?.note]).toEqual([2400, undefined, 't', undefined])
  expect([a?.started, a?.ended]).toEqual([Date.parse('2026-09-23T10:00:00Z'), 1758621600000])
  expect(f?.aliases).toEqual(['old', '7'])
  expect(f?.footer).toBe('a total')
  expect(parse('{"rows":[')).toBeUndefined()
  expect(parse('[]')).toBeUndefined()
  expect(parse('{"version":2}')).toBeUndefined()
})

// These run the plugin itself: a test's own `$` holds no `$.session`, and what the plugin does
// is what is checked.
const BAND = {
  plugin: 'manager',
  component: 'AbovePrompt',
  surface: 'terminal',
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 150, scroll: { offset: 0, bodyRows: 20 }, view: {} },
} as const

function base(on: On) {
  mock.env(on, { GIVERNY_FEED_DIR: DIR })
  on('command.register', () => ({ value: { command: 'manager' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: ME }))
  on('agent.list', () => ({ value: [] }))
  return mock.clock(on, { now: 100_000 })
}

const row = (key: string, stage = 'running') => ({ key, stage, started: 40_000, eta_s: 600 })

test('the feed is read and the band written only when something shows differently', async ($, on) => {
  const clock = base(on)
  let stamp = file(1, 10)
  let body = JSON.stringify({ rows: [row('first')] })
  let reads = 0
  let writes = 0
  on('fs.stat', () => ({ value: stamp }))
  on('fs.read', () => {
    reads += 1
    return { value: body }
  })
  on('state.set', { plugin: 'manager', key: 'view' }, (_, e, next) => {
    writes += 1
    return next(e)
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect([reads, writes]).toEqual([1, 1])

  // Polls with nothing new: no read and no write, up to the turn of the minute.
  await clock.advance(10_000)
  expect([reads, writes]).toEqual([1, 1])
  // The minute turns: the clocks move, the file is not read again.
  await clock.advance(10_000)
  expect([reads, writes]).toEqual([1, 2])

  // The file is rewritten with the same rows: read, not redrawn.
  stamp = file(2, 10)
  await clock.advance(2000)
  expect([reads, writes]).toEqual([2, 2])

  stamp = file(3, 20)
  body = JSON.stringify({ rows: [row('second')] })
  await clock.advance(2000)
  expect([reads, writes]).toEqual([3, 3])
  const ui = await $.ui.mount(BAND)
  expect((await ui.find({ type: 'Text', text: /second/ })) !== undefined).toBe(true)

  // A torn write keeps the last good read.
  stamp = file(4, 5)
  body = '{"row'
  await clock.advance(2000)
  expect(reads).toBe(4)
  expect((await ui.find({ type: 'Text', text: /second/ })) !== undefined).toBe(true)
  await ui.unmount()
})

test('with no file of its own a session reads each other file once, a minute apart at most', async ($, on) => {
  const clock = base(on)
  const read: string[] = []
  on('fs.stat', () => {
    throw new Error('missing')
  })
  on('fs.list', () => ({
    value: [
      { name: 'other.json', ...file(1, 10) },
      { name: 'other.json.tmp', ...file(1, 10) },
    ],
  }))
  on('fs.read', (_, e) => {
    read.push(e.path)
    return { value: '{"session":"someone","rows":[]}' }
  })
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect(read).toEqual([`${DIR}/other.json`])
  await clock.advance(130_000)
  expect(read).toEqual([`${DIR}/other.json`])
})

test('an alias finds the file a re-id left behind, and the session stays on it', async ($, on) => {
  const clock = base(on)
  let listed = 0
  on('fs.stat', (_, e) => {
    if (e.path === `${DIR}/${ME}.json`) throw new Error('missing')
    return { value: file(1, 10) }
  })
  on('fs.list', () => {
    listed += 1
    return { value: [{ name: 'first.json', ...file(1, 10) }] }
  })
  on('fs.read', () => ({ value: JSON.stringify({ session: 'first', aliases: [ME], rows: [row('kept')] }) }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await clock.advance(130_000)
  expect(listed).toBe(1)
  const ui = await $.ui.mount(BAND)
  expect((await ui.find({ type: 'Text', text: /kept/ })) !== undefined).toBe(true)
  await ui.unmount()
})
