import type { AgentInfo, On } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'

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
  on('ui.view', () => ({ value: {} }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ type: 'Text', text: /1 running · 1 next · 1 done/ })) !== undefined).toBe(true)
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
  expect((await ui.find({ type: 'Text', text: /0 running · 1 next · 0 done/ })) !== undefined).toBe(true)
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
  expect((await ui.find({ type: 'Text', text: /1 running · 0 next · 0 done/ })) !== undefined).toBe(true)
  expect(await ui.find({ type: 'Text', text: /a note to itself/ })).toBe(undefined)
})
