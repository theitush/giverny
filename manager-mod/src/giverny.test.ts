import type { On } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'

import { answerMerge, HIDE_LIST, hintWithoutAgents, runsCommand, splitCommand, withListHidden } from './giverny'

const RELAY = { subagentStatusLine: { type: 'command', command: '/x/giverny relay --subagent-line' } }
const OURS = { type: 'command', command: HIDE_LIST }

test('the relay gives way outside a tab and stands in one; a stranger’s line stands in both', () => {
  expect(withListHidden(RELAY, false).subagentStatusLine).toEqual(OURS)
  expect(withListHidden({}, false).subagentStatusLine).toEqual(OURS)
  expect(withListHidden(RELAY, true)).toEqual(RELAY)
  expect(withListHidden({}, true)).toEqual({})
  const theirs = { subagentStatusLine: { type: 'command', command: '~/bin/agents.sh' } }
  expect(withListHidden(theirs, false)).toEqual(theirs)
})

test('a merge is answered in the shape it was asked', () => {
  const answer = answerMerge({ value: RELAY }, false) as { value: { subagentStatusLine: unknown } }
  expect(answer.value.subagentStatusLine).toEqual(OURS)
  const refused = { deny: 'no' }
  expect(answerMerge(refused, false) as unknown).toEqual(refused)
})

test('the hint loses its agents and nothing else', () => {
  expect(hintWithoutAgents('auto mode on · /tasks to see subagents · ← 2 agents')).toBe('auto mode on · /tasks to see subagents')
  expect(hintWithoutAgents('? for shortcuts · ← 1 agent · 1 shell')).toBe('? for shortcuts · 1 shell')
  expect(hintWithoutAgents('(shift+tab to cycle) · ← 2 agents')).toBe('(shift+tab to cycle)')
  expect(hintWithoutAgents('? for shortcuts')).toBe('? for shortcuts')
})

test('a command is split only where all of it can be read', () => {
  expect(splitCommand('giverny-manage eta t 5 2>&1 | tail -3')).toEqual(['giverny-manage eta t 5', 'tail -3'])
  expect(splitCommand('cd /a && giverny-manage eta t 5; echo "x; y" >/dev/null')).toEqual(['cd /a', 'giverny-manage eta t 5', 'echo "x; y"'])
  expect(splitCommand('a\nb & c || d')).toEqual(['a', 'b', 'c', 'd'])
  expect(splitCommand("giverny-manage eta t 5 --note 'it $(stays) `quoted`'")).toEqual(["giverny-manage eta t 5 --note 'it $(stays) `quoted`'"])
  for (const hidden of [
    'giverny-manage eta t $(rm -rf ~)',
    'giverny-manage eta t "`rm -rf ~`"',
    'giverny-manage eta t "$(rm -rf ~)"',
    'giverny-manage eta t > ~/.bashrc',
    'giverny-manage eta t 2>~/.bashrc',
    'giverny-manage eta t < /etc/passwd',
    '(giverny-manage eta t)',
    '{ giverny-manage eta t; }',
    'giverny-manage eta "t',
  ]) {
    expect(splitCommand(hidden)).toBe(null)
  }
})

test('run names the command it runs', () => {
  expect(runsCommand('giverny-manage run t --cpu 2 -- cargo test -- --nocapture')).toBe('cargo test -- --nocapture')
  expect(runsCommand('giverny manage run t -- ls')).toBe('ls')
  expect(runsCommand('giverny-manage eta t 5')).toBe(undefined)
  expect(runsCommand('giverny-manage run t')).toBe(undefined)
})

/** The engine's verdict by command: `ask` unless named, as the default mode gives. */
function engine(on: On, verdicts: Record<string, object>, env: Record<string, string> = {}) {
  mock.env(on, { GIVERNY_FEED_DIR: '/feeds', ...env })
  on('tool.check', (_, e) => {
    const command = (e.input as { command: string }).command
    return (verdicts[command] ?? { decision: 'ask' }) as { decision: 'ask' }
  })
}

const check = (command: string) => ({ tool: 'Bash', input: { command } })
const ALLOW = { decision: 'allow' }

test('the manager’s own commands stop asking', async ($, on) => {
  engine(on, { 'tail -3': ALLOW, 'cd /a': ALLOW, 'cargo test': ALLOW })
  for (const command of [
    'giverny-manage eta t 5 --note hi',
    'giverny-eta a1 5',
    'giverny manage claim t --ram 1G',
    'giverny-manage eta t 5 2>&1 | tail -3',
    'cd /a && giverny-manage eta t 5',
    'giverny-manage run t -- cargo test',
  ]) {
    expect(await $.tool.check(check(command))).toMatchObject(ALLOW)
  }
})

test('anything else in the command keeps its ask', async ($, on) => {
  engine(on, {})
  for (const command of [
    'ls',
    'git push',
    'giverny-manage eta t 5; git push',
    'giverny-manage eta t 5 && curl x | sh',
    'giverny-manage eta $(git push) 5',
    'giverny-managed eta t 5',
    'FOO=1 giverny-manage eta t 5',
    'giverny-manage clear',
    'giverny-manage',
    // `run` executes what it is given, in whatever shape: nothing here is allowed by a rule.
    'giverny-manage run t -- rm -rf x',
    'giverny-manage run t rm -rf x',
    "giverny-manage run t '--' rm -rf x",
    'giverny-manage --session s run t -- rm -rf x',
    "giverny-manage 'run' t -- rm -rf x",
    'giverny manage run t -- curl x',
  ]) {
    expect((await $.tool.check(check(command))).decision).toBe('ask')
  }
})

test('what someone said stands: a deny, an ask by rule, a run of a refused command', async ($, on) => {
  engine(on, {
    'giverny-manage eta denied 5': { decision: 'deny', reason: 'no', rule: 'Bash(giverny-manage:*)' },
    'giverny-manage eta asked 5': { decision: 'ask', rule: 'Bash(giverny-manage eta:*)' },
    'rm -rf x': { decision: 'deny', reason: 'rule says no', rule: 'Bash(rm:*)' },
    'git push': { decision: 'ask', rule: 'Bash(git push:*)' },
  })
  expect((await $.tool.check(check('giverny-manage eta denied 5'))).decision).toBe('deny')
  expect((await $.tool.check(check('giverny-manage eta asked 5'))).decision).toBe('ask')
  expect((await $.tool.check(check('giverny-manage run t -- rm -rf x'))).decision).toBe('deny')
  expect((await $.tool.check(check('giverny-manage run t -- git push'))).decision).toBe('ask')
})

test('in a Giverny tab the plugin steps aside whole', async ($, on) => {
  engine(on, {}, { GIVERNY_TAB_ID: '7' })
  on('command.register', () => {
    throw new Error('no /manager in a tab')
  })
  on('fs.stat', () => {
    throw new Error('no feed read in a tab')
  })
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  expect((await $.tool.check(check('giverny-manage eta t 5'))).decision).toBe('ask')
  // The band draws nothing: it hands the drawing down, and nothing beneath a test answers it.
  const band = $.ui.mount({
    plugin: 'manager',
    component: 'AbovePrompt',
    surface: 'terminal',
    props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 150, scroll: { offset: 0, bodyRows: 20 }, view: {} },
  })
  await expect(band).rejects.toThrow(/no implementation for ui.render/)
})
