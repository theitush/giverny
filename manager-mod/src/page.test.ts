import { expect, test } from 'claude-code/testing'

import { entriesOf, entriesOfFile, inRoom, noBrief, pendingAfter, unwrapped } from './page'

const user = (text: string) => ({ role: 'user' as const, text, toolUses: [] })

test('a page lists the prompt, the words and one line per tool call, not what no person wrote', () => {
  const out = entriesOf([
    user('Fix the build'),
    user('<system-reminder>\nnot for a person</system-reminder>'),
    user('[Request interrupted by user for tool use]'),
    { role: 'assistant', text: 'On it.', toolUses: [{ tool: 'Bash', input: { command: 'cargo test' } }] },
    { role: 'assistant', text: '', toolUses: [{ tool: 'SubagentHandback', input: { message: 'It builds.' } }] },
  ])
  expect(out.map(e => [e.kind, e.text])).toEqual([
    ['prompt', 'Fix the build'],
    ['tool', 'Interrupted'],
    ['text', 'On it.'],
    ['tool', 'Bash: cargo test'],
    ['text', 'It builds.'],
  ])
})

test('a line that reached a worker mid-turn shows as the person wrote it', () => {
  const wrapped = 'The user sent a new message while you were working:\nstop after this file\n\nThis is how Claude Code surfaces messages the user sends mid-turn — within the running turn. Address it.'
  expect(unwrapped(wrapped)).toBe('stop after this file')
  expect(unwrapped('The coordinator sent a message while you were working:\nhello\n\nAddress this before completing your current task.')).toBe('hello')
  expect(unwrapped('plain words')).toBe('plain words')
})

test('a transcript file reads the same, and a line being written is skipped', () => {
  const file = [
    JSON.stringify({ type: 'user', message: { role: 'user', content: 'Do it' } }),
    JSON.stringify({ type: 'user', isMeta: true, message: { role: 'user', content: 'hidden' } }),
    JSON.stringify({ type: 'user', isMeta: true, origin: { kind: 'human' }, message: { role: 'user', content: 'The user sent a new message while you were working:\nfaster\n\nThis is how Claude Code surfaces messages the user sends mid-turn.' } }),
    JSON.stringify({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'thinking', thinking: 'hm' }, { type: 'text', text: 'Done' }, { type: 'tool_use', name: 'Read', input: { file_path: '/a' } }] } }),
    '{"type":"assistant","message":{"role":"assist',
  ].join('\n')
  expect(entriesOfFile(file).map(e => e.text)).toEqual(['Do it', 'faster', 'Done', 'Read: /a'])
})

test('a long transcript keeps its newest entries and says how many it left out', () => {
  const entries = ['a'.repeat(50), 'b'.repeat(50), 'c'.repeat(50)].map(text => ({ kind: 'text' as const, text }))
  const { shown, left } = inRoom(entries, 110)
  expect(left).toBe(1)
  expect(shown.map(e => e.text[0])).toEqual(['b', 'c'])
})

test('each prompt that lands takes the place of the oldest line sent, whatever its words', () => {
  const landed = [{ kind: 'prompt' as const, text: 'the brief' }, { kind: 'prompt' as const, text: 'changed by the manager' }]
  expect(pendingAfter(['first', 'second'], 1, landed)).toEqual(['second'])
  expect(pendingAfter(['first', 'second'], 2, landed)).toEqual(['first', 'second'])
})

test('a Next up row with no brief says what it has and how to add one', () => {
  expect(noBrief('g12', 'FEATURE: x', 'after the design')).toContain('Task: g12\n\nFEATURE: x\n\nafter the design\n\nNo brief was given')
})
