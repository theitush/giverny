import { expect, test } from 'claude-code/testing'

import type { AgentRow, Around, DisplayRow, Feed, FeedLease, FeedRow, Hold, Ledger } from '../types'

import { leaseFact, parseLedger, queuedNote, usageFact, withLedger } from './lease'
import { buildModel, clearable, dueChores, join, namesKey } from './model'
import { gist, named } from './shell-name'
import { clockAt, countdown, fmtDelta, fmtSpan, fmtTokens, minuteOf, offMs, spent, track } from './time'

// Local noon, so a clock reads the same whatever zone the machine is in.
const NOW = new Date(2026, 10, 4, 12, 0, 0).getTime()
const AGO = (min: number) => NOW - min * 60000
const row = (r: Partial<FeedRow> & { key: string }): FeedRow => ({ stage: 'running', ...r })
const agent = (id: string, description: string, status = 'running', parentId?: string): AgentRow => ({ id, description, status, parentId })
/** A worker that started `min` minutes ago. */
const worker = (id: string, description: string, min: number, more: Partial<AgentRow> = {}): AgentRow => ({ id, description, status: 'running', startedMs: AGO(min), ...more })
const feed = (...rows: FeedRow[]): Feed => ({ aliases: [], writer: 'giverny/manage', rows })
const MANAGER: Partial<Around> = { manager: true }
const lease = (l: Partial<FeedLease>): FeedLease => ({ state: 'granted', ram_mb: 0, gpus: [], vram_mb: 0, slots: [], short: [], uncollected: false, lapsed: false, ...l })
const build = (rows: FeedRow[], agents: AgentRow[] = [], around: Partial<Around> = MANAGER) => buildModel(feed(...rows), agents, NOW, around)
const of = (rows: DisplayRow[], id: string) => {
  const r = rows.find(x => x.id === id)
  if (!r) throw new Error(`no row ${id}`)
  return r
}
const hm = (min: number) => clockAt(AGO(min), NOW)

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

test('a fresh worker naming the task takes over from one that ended, or died with its session', async () => {
  const j = join([row({ key: 'a', agent_id: 'old' })], [agent('old', 'a: first', 'completed'), agent('new', 'a: again')])
  expect(j[0]?.agent?.id).toBe('new')
  // The one replaced keeps a Done row of its own.
  expect(j[1]?.row).toBeUndefined()
  expect(j[1]?.agent?.id).toBe('old')
  // A killed row's worker may still read running: it goes Done too, where it was last seen.
  const k = build(
    [row({ key: 'a', agent_id: 'old', started: AGO(60), killed: AGO(10), eta_s: 3600 })],
    [worker('old', 'a: first', 60, { lastTurnMs: AGO(40) }), worker('new', 'a: revived', 2)],
  )
  expect(of(k.rows, 'a').workerId).toBe('new')
  expect(of(k.rows, 'old').section).toBe('done')
  expect(of(k.rows, 'old').elapsed).toBe('20m')
})

test('a worker no row carries is drawn on its own, once', async () => {
  const j = join([row({ key: 'one' })], [agent('w1', 'one: fix'), agent('w2', 'look around'), agent('w3', 'one: second')])
  expect(j.map(x => x.agent?.id)).toEqual(['w1', 'w2'])
  // A plain session with no feed row at all: its workers show, Running and Done.
  const m = buildModel(undefined, [worker('w2', 'look around', 3), { ...agent('w4', 'spent', 'completed'), startedMs: AGO(9), endedMs: AGO(1) }], NOW)
  expect(m.rows.map(r => [r.section, r.key, r.title, r.elapsed, r.left, r.what, r.workerId])).toEqual([
    ['running', '', 'look around', '3m', 'no ETA', '', 'w2'],
    ['done', '', 'spent', '8m', '', 'done', 'w4'],
  ])
  expect(of(m.rows, 'w2').facts[1]).toBe('no ETA — add one: giverny-eta w2 <min>')
})

test('an agent another agent spawned is no row worker by its description', async () => {
  const j = join([row({ key: 'a', agent_id: 'w1' })], [agent('w1', 'a: fix'), agent('n1', 'a: triage', 'running', 'w1')])
  expect(j.map(x => [x.row?.key, x.agent?.id])).toEqual([
    ['a', 'w1'],
    [undefined, 'n1'],
  ])
})

test('rule 1: a feed row no worker stands behind draws only in a manager, and only Next up or a build', async () => {
  const rows = [
    row({ key: 'r', started: AGO(1), eta_s: 600 }),
    row({ key: 'n', stage: 'planned', eta_s: 600 }),
    row({ key: 'd', stage: 'done', started: AGO(9), ended: AGO(1) }),
    row({ key: 'build', stage: 'running', started: AGO(1), usage: { runs: 1 } }),
    row({ key: 'b2', stage: 'done', started: AGO(9), ended: AGO(2), lease: lease({ ram_mb: 2048 }) }),
  ]
  expect(build(rows, [], { manager: false }).rows).toEqual([])
  expect(build(rows).rows.map(r => r.id)).toEqual(['build', 'n', 'b2'])
})

test('rule 2: the clock is the worker start, moved on by its pauses, unless the row began far from it', async () => {
  const at = (r: FeedRow, w: AgentRow) => of(build([r], [w]).rows, r.key).elapsed
  // A writer stamps a little before the spawn: the worker's start wins.
  expect(at(row({ key: 'a', started: AGO(12), agent_id: 'w' }), worker('w', 'x', 10))).toBe('10m')
  // Its pauses move the worker's start on by as much as they moved `started`.
  expect(at(row({ key: 'a', started: AGO(8), spawned: AGO(12), agent_id: 'w' }), worker('w', 'x', 10))).toBe('6m')
  expect(at(row({ key: 'a', started: AGO(8), paused_s: 240, agent_id: 'w' }), worker('w', 'x', 10))).toBe('6m')
  // A later task of the same worker keeps its own clock.
  expect(at(row({ key: 'a', started: AGO(10), agent_id: 'w' }), worker('w', 'x', 60))).toBe('10m')
  // A fresh worker taking a task begun long before it counts from `started`.
  expect(at(row({ key: 'a', started: AGO(60), agent_id: 'w' }), worker('w', 'a: again', 1))).toBe('1h')
  // Tokens: the worker's over the feed's; a Next up row has none.
  const m = build([row({ key: 'a', tokens: 5, agent_id: 'w' }), row({ key: 'n', stage: 'planned', tokens: 7 })], [worker('w', 'x', 1, { tokens: 13500 })])
  expect([of(m.rows, 'a').tokens, of(m.rows, 'n').tokens]).toEqual(['13.5k', ''])
})

test('rule 5: Next up reads in the order its rows will start, an after-chain below what it waits on', async () => {
  const m = build([
    row({ key: 'free', stage: 'planned', planned: AGO(1) }),
    row({ key: 'q2', stage: 'planned', lease: lease({ state: 'queued', position: 2 }) }),
    row({ key: 'hold', stage: 'planned', held: 'a quiet machine' }),
    row({ key: 'q1', stage: 'planned', lease: lease({ state: 'queued', position: 1 }) }),
    row({ key: 'chain', stage: 'planned', after: 'free' }),
    row({ key: 'behind-hold', stage: 'planned', after: 'hold' }),
    row({ key: 'granted', stage: 'planned', lease: lease({ state: 'granted', ram_mb: 1024 }) }),
    row({ key: 'loop1', stage: 'planned', after: 'loop2' }),
    row({ key: 'loop2', stage: 'planned', after: 'loop1' }),
  ])
  expect(m.rows.map(r => r.id)).toEqual(['granted', 'q1', 'q2', 'free', 'chain', 'loop1', 'loop2', 'hold', 'behind-hold'])
})

test('rule 6: the same worker directly above draws its cells as a ditto, and rows of another batch do not', async () => {
  const m = build(
    [row({ key: 'a', title: 'A', agent_id: 'w', started: AGO(10), eta_s: 3600 }), row({ key: 'b', title: 'B', agent_id: 'w', started: AGO(10), eta_s: 3600 }), row({ key: 'c', title: 'C', agent_id: 'x', started: AGO(10), eta_s: 3600 })],
    [worker('w', 'a b', 10, { tokens: 900, activity: 'Bash: ls' }), worker('x', 'c', 10, { tokens: 900, activity: 'Bash: ls' })],
  )
  expect(m.rows.map(r => [r.id, r.elapsed, r.left, r.what, r.tokens])).toEqual([
    ['a', '10m', '~50m', 'Bash: ls', '900'],
    ['b', '"', '"', '"', '"'],
    ['c', '10m', '~50m', 'Bash: ls', '900'],
  ])
})

test('rule 7: tasks in turn close the one before, and a later Planned row waits on the worker', async () => {
  const m = build(
    [
      row({ key: 'one', agent_id: 'w', started: AGO(60), eta_s: 1800 }),
      row({ key: 'two', agent_id: 'w', started: AGO(20), eta_s: 3600 }),
      row({ key: 'three', stage: 'planned', agent_id: 'w', eta_s: 600 }),
    ],
    [worker('w', 'one: the first', 60, { tokens: 77000 })],
  )
  const [one, two, three] = [of(m.rows, 'one'), of(m.rows, 'two'), of(m.rows, 'three')]
  expect([one.section, one.what, one.elapsed, one.left, one.tokens, one.landedMs]).toEqual(['done', '→ two', '40m', '(+10m)', '', AGO(20)])
  expect([two.section, two.elapsed, two.left, two.tokens]).toEqual(['running', '20m', '~40m', '77k'])
  expect([three.section, three.what, three.why[0]]).toEqual(['next', 'after two', 'after two — starts when its worker is done with that'])
  // A count frozen when it landed is drawn in its place.
  const frozen = build([row({ key: 'one', agent_id: 'w', stage: 'done', started: AGO(60), ended: AGO(20), task_tokens: 1234 }), row({ key: 'two', agent_id: 'w', started: AGO(20) })], [worker('w', 'one', 60)])
  expect(of(frozen.rows, 'one').tokens).toBe('1.2k')
})

test('rule 8: a Next up row says what it waits for, first that applies', async () => {
  const words = (r: FeedRow, more: FeedRow[] = []) => of(build([r, ...more]).rows, r.key).what
  const queuedRam = lease({ state: 'queued', ram_mb: 3072, short: ['ram'], behind: '899x', position: 2 })
  expect(words(row({ key: 'n', stage: 'planned', held: 'a quiet machine', lease: queuedRam, after: 'x' }), [row({ key: 'x', stage: 'planned' })])).toBe('held: a quiet machine')
  expect(words(row({ key: 'n', stage: 'planned', lease: queuedRam, after: 'x' }), [row({ key: 'x', stage: 'planned' })])).toBe('queued for 3G behind 899x')
  expect(words(row({ key: 'n', stage: 'planned', after: 'x' }), [row({ key: 'x', stage: 'planned' })])).toBe('after x')
  // Its `after` row Done, or gone from the feed, is no wait.
  expect(words(row({ key: 'n', stage: 'planned', after: 'x' }), [row({ key: 'x', stage: 'done', ended: AGO(1), started: AGO(9) })])).toBe('')
  expect(words(row({ key: 'n', stage: 'planned', after: 'gone' }))).toBe('')
  expect(words(row({ key: 'n', stage: 'planned', lease: lease({ uncollected: true, ram_mb: 1024 }) }))).toBe('granted, waiting for its manager')
  expect(queuedNote(lease({ state: 'queued', short: ['cpu'] }))).toBe('queued for CPU')
  expect(queuedNote(lease({ state: 'queued', short: ['cpu', 'ram'], ram_mb: 1536, behind: 'a' }))).toBe('queued for CPU, 1.5G behind a')
  expect(queuedNote(lease({ state: 'queued', lapsed: true, ram_mb: 1024, short: ['ram'] }))).toBe('queued, grant lapsed, waiting for its manager')
})

test('rule 9: a Running row nobody works on is drawn under Next up, saying why', async () => {
  const next = (r: FeedRow, agents: AgentRow[] = []) => {
    const shown = of(build([r], agents).rows, r.key)
    return [shown.section, shown.what]
  }
  expect(next(row({ key: 'a', started: AGO(5), eta_s: 600 }))).toEqual(['next', 'no worker yet'])
  // Started a moment ago, a spawn may yet come: it is drawn nowhere, as Giverny's code has it.
  expect(build([row({ key: 'a', started: AGO(1) })]).rows).toEqual([])
  expect(next(row({ key: 'a', started: AGO(1), paused_since: AGO(1) }))).toEqual(['next', `paused since ${hm(1)}`])
  expect(next(row({ key: 'a', started: AGO(1), held: 'wait' }))).toEqual(['next', 'held: wait'])
  // Its worker handed it back and its manager paused it, or the pause ended the worker's turn.
  const done = { ...agent('w', 'a: x', 'completed'), startedMs: AGO(30), endedMs: AGO(20) }
  expect(next(row({ key: 'a', agent_id: 'w', started: AGO(30), paused_since: AGO(10) }), [done])).toEqual(['next', 'paused · worker finished'])
  expect(next(row({ key: 'a', agent_id: 'w', started: AGO(30), paused_since: AGO(25) }), [done])).toEqual(['next', `paused since ${hm(25)}`])
  // Not paused, it stays Running with when the turn ended, and the wait it marked.
  const ended = build([row({ key: 'a', agent_id: 'w', started: AGO(30), waiting_since: AGO(21), note: 'machine time' })], [done])
  expect([of(ended.rows, 'a').section, of(ended.rows, 'a').what, of(ended.rows, 'a').finished]).toEqual(['running', `turn ended ${hm(20)} · waiting: machine time`, true])
  // A worker given this task and not woken yet ended the task before it.
  const idle = { ...agent('w', 'other', 'completed'), startedMs: AGO(60), endedMs: AGO(30) }
  expect(of(build([row({ key: 'a', agent_id: 'w', started: AGO(1) })], [idle]).rows, 'a').section).toBe('running')
  expect(next(row({ key: 'a', agent_id: 'w', started: AGO(5) }), [idle])).toEqual(['next', 'no worker yet'])
  // A failed worker's row keeps Running, its end in NOW.
  const failed = { ...agent('w', 'a: x', 'failed'), startedMs: AGO(30), endedMs: AGO(20) }
  const f = build([row({ key: 'a', agent_id: 'w', started: AGO(30), eta_s: 3600 })], [failed])
  expect([of(f.rows, 'a').section, of(f.rows, 'a').what, of(f.rows, 'a').elapsed]).toEqual(['running', 'stopped: failed', '10m'])
})

test('rule 9: what the rows ask of the manager, once due and not yet told', async () => {
  const done = { ...agent('w', 'a: x', 'completed'), startedMs: AGO(30), endedMs: AGO(4) }
  const rows = [
    row({ key: 'a', agent_id: 'w', started: AGO(30) }),
    row({ key: 'n', stage: 'planned', planned: AGO(7) }),
    row({ key: 'soon', stage: 'planned', planned: AGO(2) }),
    row({ key: 'nw', started: AGO(5) }),
    row({ key: 'h', stage: 'planned', planned: AGO(9), held: 'x' }),
  ]
  const due = dueChores(feed(...rows), [done], NOW, MANAGER)
  expect(due.chores).toEqual([
    { key: 'a', kind: 'land', since: AGO(4), how: 'finished' },
    { key: 'nw', kind: 'worker', since: AGO(5) },
    { key: 'n', kind: 'start', since: AGO(7) },
  ])
  // Told once: the row carries the stamp.
  const told = dueChores(feed(row({ key: 'a', agent_id: 'w', started: AGO(30), land_nudged: AGO(4) })), [done], NOW, MANAGER)
  expect(told.chores).toEqual([])
  // A session that is no manager is never told of a row with no worker.
  expect(dueChores(feed(...rows), [], NOW, { manager: false }).chores).toEqual([])
  // Asleep on its own shell: nothing to land. Paused with its worker at work: still finishing.
  const asleep = dueChores(feed(row({ key: 'a', agent_id: 'w', started: AGO(30) })), [done], NOW, { manager: true, shells: { w: [{ id: 's', cmd: 'sleep 600', startedMs: AGO(9) }] } })
  expect(asleep.chores).toEqual([])
  const still = dueChores(feed(row({ key: 'a', agent_id: 'w', started: AGO(30), paused_since: AGO(1) })), [worker('w', 'a: x', 30)], NOW, MANAGER)
  expect(still.finishing).toEqual(['a'])
})

test('rule 10: an agent a worker spawned hangs under its row, and folds away', async () => {
  const rows = [row({ key: 'a', agent_id: 'w', started: AGO(30), title: 'A' }), row({ key: 'b', title: 'B', agent_id: 'x', started: AGO(5) })]
  const agents = [
    worker('w', 'a: go', 30),
    worker('x', 'b: go', 5),
    worker('n1', 'Triage group A', 10, { parentId: 'w' }),
    { ...agent('n2', 'Second', 'completed', 'n1'), startedMs: AGO(9), endedMs: AGO(8) },
  ]
  const open = build(rows, agents)
  expect(open.nested).toBe(true)
  expect(open.rows.map(r => [r.id, r.section, r.own, r.depth, r.head])).toEqual([
    ['a', 'running', 'running', 0, '▾ '],
    ['n1', 'running', 'running', 1, '   ▾ '],
    ['n2', 'running', 'done', 2, '      └ '],
    ['b', 'running', 'running', 0, '  '],
  ])
  expect([open.running, open.done]).toEqual([4, 0])
  expect(of(open.rows, 'n1').why.at(-1)).toBe('spawned by a')
  expect(of(open.rows, 'a').fold?.note).toBe('')
  const shut = build(rows, agents, { manager: true, folded: ['a'] })
  expect(shut.rows.map(r => r.id)).toEqual(['a', 'b'])
  expect(of(shut.rows, 'a').fold).toEqual({ id: 'a', folded: true, under: 2, running: 1, note: '+2, 1 running', shortNote: '+2' })
  expect(of(shut.rows, 'a').head).toBe('▸ ')
})

test('the words of a Running row: paused, killed, asleep, stopped, a limit, overdue', async () => {
  const one = (r: Partial<FeedRow>, a: AgentRow, more: Partial<Around> = {}) => of(build([row({ key: 'a', agent_id: 'w', ...r })], [a], { manager: true, ...more }).rows, 'a')
  const w = worker('w', 'a: x', 30, { activity: 'Bash: cargo test' })
  expect(one({ started: AGO(30), eta_s: 3600 }, w).what).toBe('Bash: cargo test')
  // Paused with its worker still at work: only the clock.
  const p = one({ started: AGO(30), eta_s: 3600, paused_since: AGO(10) }, w)
  expect([p.what, p.elapsed, p.left]).toEqual(['paused · worker still finishing', '20m', '~40m'])
  expect(p.why[2]).toBe(`paused since ${hm(10)}, its worker still finishing its turn: only its clock is paused until it stops`)
  // Killed with its session: stopped where it went off.
  const k = one({ started: AGO(30), eta_s: 3600, paused_since: AGO(10), killed: AGO(5), went_off: AGO(10) }, { ...w, lastTurnMs: AGO(11) })
  expect([k.section, k.what, k.elapsed, k.left]).toEqual(['running', 'killed', '20m', '~40m'])
  expect(k.why[2]).toBe(`killed: Giverny, WSL or its manager session went off at ${hm(10)}, and its worker with it; its clock stopped there until its manager revives it (a fresh worker, then resume)`)
  // Asleep on a shell of its own.
  const sleeping = { ...agent('w', 'a: x', 'completed'), startedMs: AGO(30), endedMs: AGO(8) }
  const shells = { w: [{ id: 'bjj', cmd: 'cd ~/poly; T=1; npx wrangler deploy > $T/d.log 2>&1; echo $?', startedMs: AGO(30) }] }
  const s = one({ started: AGO(30), waiting_since: AGO(8), note: 'the deploy' }, sleeping, { shells })
  expect([s.what, s.asleep, s.finished]).toEqual(['asleep · wrangler deploy (30m) · wakes when it ends · waiting: the deploy', true, true])
  expect(s.why[1]).toBe(`its worker is asleep since ${hm(8)}: its turn ended while its own background shell runs: \`wrangler deploy\` (30m in, shell bjj). Claude Code wakes it when that ends; nothing to land yet`)
  const two = { w: [...shells.w, { id: 'b2', cmd: 'sleep 9', startedMs: AGO(1) }] }
  expect(one({ started: AGO(30) }, sleeping, { shells: two }).what).toBe('asleep · wrangler deploy (30m) +1 · wakes when one ends')
  // Stopped on an API error: the clocks hold, the warning is the person's.
  const stopped = { ...w, stops: [{ fromMs: AGO(10), reason: 'no network' }] }
  const st = one({ started: AGO(30), eta_s: 3600 }, stopped)
  expect([st.what, st.elapsed, st.left]).toEqual(['stopped: no network', '20m', '~40m'])
  // Continued: the stopped span stays off the clock.
  const on = one({ started: AGO(30), eta_s: 3600 }, { ...w, stops: [{ fromMs: AGO(20), toMs: AGO(10), reason: 'no network' }] })
  expect([on.what, on.elapsed, on.left]).toEqual(['Bash: cargo test', '20m', '~40m'])
  // Out of a usage limit: both clocks held, and the row says until when.
  const reopens = NOW + 3600_000
  const lim = one({ started: AGO(30), eta_s: 3600 }, w, { limit: { window: '5h', reopensMs: reopens }, holds: [{ sinceMs: AGO(10) }] })
  expect([lim.what, lim.elapsed, lim.left]).toEqual([`5h limit → ${clockAt(reopens, NOW)}`, '20m', '~40m'])
  // A sleep comes off every clock, and off a hold's span only once.
  const off = one({ started: AGO(30), eta_s: 3600 }, w, { sleeps: [{ since: AGO(20), until: AGO(10) }], holds: [{ sinceMs: AGO(15), untilMs: AGO(5) }] })
  expect([off.elapsed, off.left]).toEqual(['15m', '~45m'])
  // A row that carries its pauses: the pane's own hold is not taken off again.
  const carried = one({ started: AGO(30), eta_s: 3600, paused_s: 60 }, w, { holds: [{ sinceMs: AGO(15), untilMs: AGO(5) }] })
  expect(carried.elapsed).toBe('29m')
  // Overdue.
  const late = one({ started: AGO(30), eta_s: 600 }, w)
  expect([late.left, late.over]).toEqual(['-~20m', true])
  expect(late.why[2]).toBe('estimate 10m: 20m over it')
  // The worker's later figure, less the work since.
  const fig = one({ started: AGO(30), eta_s: 600, left_s: 900, left_at: AGO(5) }, w)
  expect([fig.left, fig.over]).toEqual(['~10m', false])
})

test("a writer that pauses by moving started holds its row at the file's write time", async () => {
  const f: Feed = { aliases: [], writer: 'someone', writtenMs: AGO(4), rows: [row({ key: 'a', started: AGO(20), paused_since: AGO(10), usage: { runs: 1 } })] }
  const run = { manager: true, runs: ['a'] }
  expect(of(buildModel(f, [], NOW, run).rows, 'a').elapsed).toBe('16m')
  const own: Feed = { ...f, writer: 'giverny/manage' }
  expect(of(buildModel(own, [], NOW, run).rows, 'a').elapsed).toBe('10m')
})

test('agent ETAs: a worker with no row counts down from its own figure, and lands against its first', async () => {
  const etas = { w: { leftS: 600, atMs: AGO(5), firstLeftS: 300, firstAtMs: AGO(8) } }
  const running = buildModel(undefined, [worker('w', 'Explore the code', 10)], NOW, { etas })
  expect(of(running.rows, 'w').left).toBe('~5m')
  const over = buildModel(undefined, [worker('w', 'Explore the code', 20)], NOW, { etas: { w: { leftS: 60, atMs: AGO(5), firstLeftS: 60, firstAtMs: AGO(5) } } })
  expect([of(over.rows, 'w').left, of(over.rows, 'w').over]).toEqual(['-~4m', true])
  // A Done row: the work since its first figure, against that figure.
  const done = buildModel(undefined, [{ ...agent('w', 'Explore', 'completed'), startedMs: AGO(20), endedMs: AGO(1) }], NOW, { etas })
  expect(of(done.rows, 'w').left).toBe('(+2m)')
  expect(of(done.rows, 'w').facts).toEqual(['done', 'took 19m', '(+2m) vs estimate 5m'])
  // A feed row that holds the worker wins: its key, its estimate.
  const held = build([row({ key: 'k', agent_id: 'w', started: AGO(10), eta_s: 3600 })], [worker('w', 'x', 10)], { manager: true, etas })
  expect(of(held.rows, 'k').left).toBe('~50m')
  // A row a worker started itself under the first version is drawn with no id.
  const old = build([row({ key: 'agent-a1b2', agent_id: 'w', follows_worker: true, started: AGO(1) })], [worker('w', 'look', 1)])
  expect([of(old.rows, 'agent-a1b2').key, of(old.rows, 'agent-a1b2').title]).toEqual(['', 'look'])
  const gone = build([row({ key: 'agent-a1b2', agent_id: 'w', follows_worker: true, started: AGO(9) })], [{ ...agent('w', 'look', 'completed'), startedMs: AGO(9), endedMs: AGO(1) }])
  expect(of(gone.rows, 'agent-a1b2').section).toBe('done')
})

test("the Done row's ETA cell, and what it was measured against", async () => {
  const done = (r: Partial<FeedRow>) => of(build([row({ key: 'd', stage: 'done', started: AGO(40), ended: AGO(10), usage: { runs: 1 }, ...r })]).rows, 'd')
  const facts = (d: DisplayRow) => d.facts.find(x => x.includes(' vs '))
  expect(done({ eta_delta_s: 300, eta_s: 1 }).left).toBe('(+5m)')
  expect(facts(done({ eta_delta_s: 300 }))).toBe('(+5m) vs estimate')
  // Against the worker's first re-estimate, not the latest figure.
  const w = done({ reest_s: 1200, reest_at_s: 300, eta_first_s: 60, eta_s: 60 })
  expect([w.left, facts(w)]).toEqual(['(+5m)', "(+5m) vs worker's 20m"])
  // Else the dispatcher's first figure, else its latest.
  const e = done({ eta_first_s: 2100, eta_s: 60 })
  expect([e.left, facts(e)]).toEqual(['(-5m)', '(-5m) vs estimate 35m'])
  expect(done({ eta_s: 1800 }).left).toBe('(±0m)')
  // Its waits and what the machine slept through are not work.
  expect(done({ eta_s: 1800, wait_s: 300 }).left).toBe('(-5m)')
  const asleep = of(build([row({ key: 'd', stage: 'done', started: AGO(40), ended: AGO(10), eta_s: 1800, usage: { runs: 1 } })], [], { manager: true, sleeps: [{ since: AGO(20), until: AGO(15) }] }).rows, 'd')
  expect([asleep.left, asleep.elapsed]).toEqual(['(-5m)', '25m'])
  // No estimate, no end: blank.
  expect(done({}).left).toBe('')
  expect(done({ ended: undefined, eta_s: 60 }).left).toBe('')
})

test('the Review line, and a Done row in full', async () => {
  const d = of(build([row({ key: 'd', stage: 'done', started: AGO(40), ended: AGO(10), eta_s: 1800, landing: 'Review — ita', review: 'ita — the ranking — branch search', usage: { runs: 2, peak_mb: 2150, cpu_s: 45, cap_ram_mb: 3072, oom_kills: 1, oom_open: false }, brief: '/b.md', open: 'claude --resume x' })]).rows, 'd')
  expect(d.review).toBe('ita — the ranking — branch search')
  expect(d.what).toBe('Review — ita')
  expect(d.why).toEqual(['Done: Review — ita; took 30m, on its estimate 30m', 'awaiting review: ita — the ranking — branch search'])
  expect(d.facts).toEqual(['Review — ita', 'took 30m', '(±0m) vs estimate 30m', 'peak 2.1G of 3G, 45s CPU, OOM-killed x1 earlier'])
  expect([d.brief, d.open, d.oom]).toEqual(['/b.md', 'claude --resume x', false])
  const noReview = of(build([row({ key: 'd', stage: 'done', started: AGO(40), ended: AGO(10), landing: 'Done', usage: { runs: 1 } })]).rows, 'd')
  expect(noReview.why).toEqual(['Done; took 30m, no estimate'])
})

test('which Done rows show: done_rows, and clear-done hiding what landed before it', async () => {
  const rows = [row({ key: 'd1', stage: 'done', started: AGO(50), ended: AGO(40), agent_id: 'w1' }), row({ key: 'd2', stage: 'done', started: AGO(30), ended: AGO(20), agent_id: 'w2' }), row({ key: 'undated', stage: 'done', agent_id: 'w3' })]
  const agents = [1, 2, 3].map(n => ({ ...agent(`w${n}`, `d${n}`, 'completed') }))
  const ids = (around: Partial<Around>) => build(rows, agents, around).rows.map(r => r.id)
  // Newest landed first, undated last.
  expect(ids({ doneRows: 'all' })).toEqual(['d2', 'd1', 'undated'])
  expect(ids({ doneRows: 1 })).toEqual(['d2'])
  expect(ids({ doneRows: 'hide' })).toEqual([])
  // Cleared: what landed by then goes, however it was written, and a finished worker with it.
  expect(ids({ clearedMs: AGO(30), dismissed: ['w1', 'w3'] })).toEqual(['d2'])
  const later = build([], [{ ...agent('old', 'spent', 'completed') }, { ...agent('new', 'fresh', 'completed') }], { dismissed: ['old'] })
  expect(later.rows.map(r => r.id)).toEqual(['new'])
  // A finished worker still behind a Running row is not cleared: that row is not done.
  expect(clearable([row({ key: 'held', agent_id: 'a' }), row({ key: 'gone', stage: 'done', agent_id: 'b' })], [agent('a', 'x', 'completed'), agent('b', 'y', 'completed'), agent('c', 'z', 'completed'), agent('d', 'w')])).toEqual(['b', 'c'])
  // A running agent under a Done row dropped stands on its own.
  const under = build([row({ key: 'd', stage: 'done', agent_id: 'w', started: AGO(9), ended: AGO(1) })], [agent('w', 'd', 'completed'), worker('n', 'still', 1, { parentId: 'w' })], { doneRows: 'hide' })
  expect(under.rows.map(r => [r.id, r.depth])).toEqual([['n', 0]])
})

test('the ledger decides: a lease gone from it is gone from the row', async () => {
  const me = 'S'
  const ledger: Ledger = {
    leases: [
      { id: 'S:held', session: me, task: 'held', ram_mb: 3072, gpus: [0], vram_mb: 8192, slots: ['cargo:/x'], granted_ms: AGO(5), uncollected: false },
      { id: 'O:other', session: 'O', task: 'holder', ram_mb: 8192, gpus: [], vram_mb: 0, slots: ['cargo:/y'], uncollected: false },
    ],
    queue: [
      { session: 'O', task: 'first', ram_mb: 1024, gpu: 0, vram_mb: 0, slots: ['cargo:/y'], queued_ms: AGO(9), lapsed: false },
      { session: me, task: 'waits', ram_mb: 2048, gpu: 0, vram_mb: 0, slots: ['cargo:/y'], queued_ms: AGO(3), lapsed: false },
    ],
  }
  const rows = withLedger(
    [
      row({ key: 'held', lease: lease({ state: 'queued' }) }),
      row({ key: 'waits', lease: lease({ state: 'queued', short: ['slot cargo:/y'], behind: 'stale' }) }),
      row({ key: 'released', lease: lease({ ram_mb: 1024 }) }),
      row({ key: 'first' }),
    ],
    [me],
    ledger,
  )
  expect(leaseFact(rows[0]?.lease as FeedLease)).toBe('holds 3G, gpu0 8G, slot cargo:/x')
  expect(leaseFact(rows[1]?.lease as FeedLease)).toBe('queued #2 for a slot behind first')
  expect(rows[2]?.lease).toBeUndefined()
  // A row matches only under this session's ids.
  expect(rows[3]?.lease).toBeUndefined()
  // A copy from before it named what is short: the task it named while still in the way, else the slot's holder.
  const old = withLedger([row({ key: 'waits', lease: lease({ state: 'queued', behind: 'gone' }) })], [me], ledger)
  expect(old[0]?.lease?.behind).toBe('holder')
  // Without a reading the row's own copy stands.
  const copy = build([row({ key: 'n', stage: 'planned', lease: lease({ state: 'queued', ram_mb: 1024, short: ['ram'] }) })])
  expect(of(copy.rows, 'n').lease).toBe('queued for 1G')
  const drawn = buildModel(feed(row({ key: 'n', stage: 'planned', lease: lease({ state: 'queued' }) })), [], NOW, { manager: true, sessions: [me], ledger })
  expect(of(drawn.rows, 'n').what).toBe('')
  // What a row holds is said on it, smaller and uncollected included.
  expect(leaseFact(lease({ state: 'smaller', ram_mb: 2560, wanted_ram_mb: 3072 }))).toBe('holds 2.5G (asked 3G)')
  expect(leaseFact(lease({ ram_mb: 1024, uncollected: true }))).toBe('holds 1G; granted, waiting for its manager')
  // The file: expired entries are gone, and a newer version is refused.
  const parsed = parseLedger(
    JSON.stringify({
      version: 1,
      leases: [
        { id: 'a', session: 's', task: 'live', ram_mb: 512, gpus: [], vram_mb: 0, slots: [], granted_at: '2026-11-04T10:00:00Z', heartbeat_at: new Date(AGO(1)).toISOString() },
        { id: 'b', session: 's', task: 'dead', ram_mb: 512, gpus: [], vram_mb: 0, slots: [], granted_at: '2026-11-04T10:00:00Z', heartbeat_at: new Date(AGO(21)).toISOString() },
      ],
      queue: [{ id: 'c', session: 's', task: 'w', ram_mb: 1, gpu: 0, vram_mb: 0, slots: [], queued_at: new Date(AGO(1)).toISOString(), heartbeat_at: new Date(AGO(1)).toISOString(), lapsed: { at: 1 } }],
    }),
    NOW,
  )
  expect(parsed?.leases.map(l => l.task)).toEqual(['live'])
  expect(parsed?.queue[0]?.lapsed).toBe(true)
  expect(parseLedger('{"version":2}', NOW)).toBeUndefined()
  expect(parseLedger('torn', NOW)).toBeUndefined()
})

test('a row with a run going stays Running with no worker, and an OOM still news is marked', async () => {
  const m = build([row({ key: 'integ-build', started: AGO(9), eta_s: 3600, usage: { runs: 1, oom_kills: 1, oom_open: true } })], [], { manager: true, runs: ['integ-build'] })
  const r = of(m.rows, 'integ-build')
  expect([r.section, r.oom, r.left]).toEqual(['running', true, '~51m'])
  expect(r.why[0]).toBe(`Running: no worker; its own commands run since ${hm(9)}`)
  expect(r.facts.at(-1)).toBe('OOM-killed x1')
  expect(usageFact({ peak_mb: 512, cpu_s: 300 })).toBe('peak 0.5G, 5m CPU')
})

test('the panel writes durations as its ETA column does', async () => {
  expect([0, 29, 30, 300, 3780, 3600, 98_000].map(fmtSpan)).toEqual(['0m', '0m', '1m', '5m', '1h3m', '1h', '1d3h13m'])
  expect(fmtSpan(-300)).toBe('-5m')
  expect([300, -4800, 0, 10].map(fmtDelta)).toEqual(['(+5m)', '(-1h20m)', '(±0m)', '(±0m)'])
  expect([1200, -720, -10].map(countdown)).toEqual(['~20m', '-~12m', '~0m'])
  expect([842, 13_500, 124_800, 1_200_000, 999_950].map(fmtTokens)).toEqual(['842', '13.5k', '124.8k', '1.2M', '1M'])
  expect(minuteOf(minuteOf(NOW) + 59_000)).toBe(minuteOf(NOW))
  expect(clockAt(new Date(2026, 10, 4, 9, 5).getTime(), NOW)).toBe('09:05')
  expect(clockAt(new Date(2026, 10, 9, 8, 0).getTime(), NOW)).toBe('Mon 08:00')
  expect(offMs([{ since: 10, until: 30 }, { since: 20, until: 40 }, { since: 90, until: 200 }], 0, 100)).toBe(40)
})

test('a usage limit is a window spent with a reset to come, held until it reopens', async () => {
  const at = (ms: number) => new Date(ms).toISOString()
  expect(spent([{ kind: 'five_hour', percentUsed: 99, resetsAt: at(NOW + 1000) }], NOW)).toBeUndefined()
  expect(spent([{ kind: 'five_hour', percentUsed: 100, resetsAt: at(NOW - 1000) }], NOW)).toBeUndefined()
  expect(spent([{ kind: 'five_hour', percentUsed: 100, resetsAt: at(NOW + 1000) }, { kind: 'seven_day', percentUsed: 100, resetsAt: at(NOW + 9000) }], NOW)).toEqual({ window: '7d', reopensMs: NOW + 9000 })
  const holds: Hold[] = []
  track(holds, { window: '5h', reopensMs: NOW + 1000 }, NOW)
  track(holds, { window: '5h', reopensMs: NOW + 1000 }, NOW + 500)
  expect(holds).toEqual([{ sinceMs: NOW, reopensMs: NOW + 1000 }])
  // Closed late, it ends at the reset it named.
  track(holds, undefined, NOW + 5000)
  expect(holds).toEqual([{ sinceMs: NOW, reopensMs: NOW + 1000, untilMs: NOW + 1000 }])
})

test('a shell is named past its setup', async () => {
  expect(gist('cd ~/poly; T=/tmp/x; npx wrangler deploy > $T/d.log 2>&1; echo $?')).toBe('wrangler deploy')
  expect(gist('giverny-manage run g1 --ram 3G -- cargo test --workspace')).toBe('cargo test')
  expect(gist('sleep 600')).toBe('sleep 600')
  expect(gist("cat > /tmp/f <<'EOF'\nnoise here\nEOF\npython3 /tmp/sim.py --fast")).toBe('python3 sim.py')
  expect(gist('/tmp/x/run.sh wt178 now')).toBe('run.sh wt178 now')
  expect(gist('timeout 600 nice -n 5 make all')).toBe('make all')
  expect(named([{ id: 'a', cmd: 'sleep 40', startedMs: NOW - 30_000 }], NOW)).toBe('sleep 40 (30s)')
})
