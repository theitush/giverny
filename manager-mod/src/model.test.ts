import { expect, test } from 'claude-code/testing'

import type { AgentRow, FeedRow } from '../types'

import { buildModel, join, namesKey } from './model'
import { dur, elapsed, left, minuteOf } from './time'

const NOW = Date.parse('2026-11-04T10:00:00Z')
const AGO = (min: number) => NOW - min * 60000
const row = (r: Partial<FeedRow> & { key: string }): FeedRow => ({ stage: 'running', ...r })
const agent = (id: string, description: string, status = 'running', parentId?: string): AgentRow => ({ id, description, status, parentId })

test('a key is named as a whole word', async () => {
  expect(namesKey('Work demo#82 open direct', 'demo#82')).toBe(true)
  expect(namesKey('owner/demo#82', 'demo#82')).toBe(true)
  expect(namesKey('mgr-base: the home', 'mgr-base')).toBe(true)
  expect(namesKey('Work demo#820', 'demo#82')).toBe(false)
  expect(namesKey('xdemo#82', 'demo#82')).toBe(false)
  expect(namesKey('my-mgr-base', 'mgr-base')).toBe(false)
})

test('a row finds its worker by id, then by a description that names its key', async () => {
  const byId = join([row({ key: 'a', agent_id: 'w1' })], [agent('w1', 'anything')])
  expect(byId[0]?.agent?.id).toBe('w1')
  expect(byId.length).toBe(1)
  const byName = join([row({ key: 'a' })], [agent('w1', 'a: fix it')])
  expect(byName[0]?.agent?.id).toBe('w1')
  // A Running row wants the worker still running; a Done one a finished one.
  const both = [agent('old', 'a: first', 'completed'), agent('new', 'a: again')]
  expect(join([row({ key: 'a' })], both)[0]?.agent?.id).toBe('new')
  expect(join([row({ key: 'a', stage: 'done' })], both)[0]?.agent?.id).toBe('old')
})

test('a fresh worker naming the task takes over from one that ended', async () => {
  const j = join([row({ key: 'a', agent_id: 'old' })], [agent('old', 'a: first', 'completed'), agent('new', 'a: again')])
  expect(j[0]?.agent?.id).toBe('new')
  // The one replaced keeps a Done row of its own.
  expect(j[1]?.row).toBeUndefined()
  expect(j[1]?.agent?.id).toBe('old')
})

test('a worker no row carries is drawn on its own, once', async () => {
  const j = join([row({ key: 'one' })], [agent('w1', 'one: fix'), agent('w2', 'look around'), agent('w3', 'one: second')])
  expect(j.map(x => x.agent?.id)).toEqual(['w1', 'w2'])
  const m = buildModel({ aliases: [], rows: [] }, [agent('w2', 'look around'), agent('w4', 'spent', 'completed')], NOW)
  expect(m.rows.map(r => [r.section, r.key, r.title, r.what, r.workerId])).toEqual([
    ['running', '', 'look around', '', 'w2'],
    ['done', '', 'spent', 'completed', 'w4'],
  ])
})

test('an agent another agent spawned is no row worker by its description', async () => {
  const j = join([row({ key: 'a', agent_id: 'w1' })], [agent('w1', 'a: fix'), agent('n1', 'a: triage', 'running', 'w1')])
  expect(j.map(x => [x.row?.key, x.agent?.id])).toEqual([
    ['a', 'w1'],
    [undefined, 'n1'],
  ])
})

test('the sections read Running, Next up, then Done newest first, with their text', async () => {
  const m = buildModel(
    {
      aliases: [],
      rows: [
        row({ key: 'd1', stage: 'done', started: AGO(30), ended: AGO(20), eta_first_s: 300, landing: 'Done' }),
        row({ key: 'd2', stage: 'done', started: AGO(30), ended: AGO(5) }),
        row({ key: 'n', stage: 'planned', eta_s: 600, held: 'a quiet machine' }),
        row({ key: 'r', started: AGO(10), eta_s: 300, note: 'building' }),
      ],
    },
    [],
    NOW,
  )
  expect(m.rows.map(r => r.key)).toEqual(['r', 'n', 'd2', 'd1'])
  expect([m.running, m.next, m.done]).toEqual([1, 1, 2])
  const [r, n, , d1] = m.rows
  expect([r?.elapsed, r?.left, r?.over, r?.what]).toEqual(['10m', 'over by 5m', true, 'building'])
  expect([n?.left, n?.what]).toEqual(['~10m', 'held: a quiet machine'])
  expect([d1?.elapsed, d1?.left, d1?.what, d1?.over]).toEqual(['10m', 'est 5m', 'Done', false])
})

test('a running row counts down, then says how far over it is', async () => {
  const started = AGO(10)
  expect(left(row({ key: 'a', started, eta_s: 1800 }), NOW)).toBe('~20m left')
  expect(left(row({ key: 'a', started, eta_s: 300 }), NOW)).toBe('over by 5m')
  // The worker's own later figure wins over the first estimate.
  expect(left(row({ key: 'a', started, eta_s: 300, left_s: 600, left_at: AGO(2) }), NOW)).toBe('~8m left')
  expect(left(row({ key: 'a' }), NOW)).toBe('no ETA')
  expect(left(row({ key: 'a', started, eta_s: 300, paused_since: AGO(1) }), NOW)).toBe('paused')
  // A paused row's clock holds where it stopped.
  expect(elapsed(row({ key: 'a', started, paused_since: AGO(4) }), NOW)).toBe('6m')
})

test('durations read as the panel says them, and every clock ticks on the minute', async () => {
  expect(dur(30)).toBe('<1m')
  expect(dur(720)).toBe('12m')
  expect(dur(4500)).toBe('1h15')
  expect(minuteOf(NOW + 59_000)).toBe(NOW)
})
