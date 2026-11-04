import { expect, test } from 'claude-code/testing'

import { parseDoneRows, parseEtas, parseShells, parseSleeps } from './live'
import { firstStamp, foldTranscript, mergeStops, stopReason } from './transcript'

const line = (o: object) => JSON.stringify(o)
const turn = (timestamp: string, message: object, more: object = {}) => line({ type: 'assistant', timestamp, message, ...more })

test('a transcript says when its worker wrote, what it does and how much context it carries', async () => {
  const text = [
    '{"cut in half',
    line({ type: 'user', timestamp: '2026-11-04T10:00:00Z', message: { role: 'user', content: 'go assistant' } }),
    turn('2026-11-04T10:01:00Z', { model: 'claude-haiku-4', usage: { input_tokens: 10, cache_read_input_tokens: 90 }, content: [{ type: 'tool_use', name: 'Bash', input: { command: 'cargo   test\n-p auth' } }] }),
    turn('2026-11-04T10:02:00Z', { model: 'claude-haiku-4', usage: { input_tokens: 5, cache_creation_input_tokens: 195, iterations: [{ type: 'message', input_tokens: 150 }, { type: 'compaction', input_tokens: 9 }] }, content: [{ type: 'text', text: 'ok' }] }),
    line({ type: 'user', timestamp: '2026-11-04T10:03:00Z', message: { content: [] } }),
  ].join('\n')
  const f = foldTranscript(text)
  expect([f.lastMs, f.lastTurnMs]).toEqual([Date.parse('2026-11-04T10:03:00Z'), Date.parse('2026-11-04T10:02:00Z')])
  expect([f.activity, f.tokens, f.model]).toEqual(['Bash: cargo test -p auth', 150, 'claude-haiku-4'])
  expect(f.stops).toEqual([])
  expect(firstStamp(line({ type: 'user', timestamp: '2026-11-04T10:00:00Z' }))).toBe(Date.parse('2026-11-04T10:00:00Z'))
  expect(firstStamp('torn')).toBeUndefined()
})

test('an API error is a span it stood stopped, until its next real turn', async () => {
  const error = (at: string, text: string, extra: object = {}) => turn(at, { model: '<synthetic>', content: [{ type: 'text', text }] }, { isApiErrorMessage: true, ...extra })
  const open = foldTranscript(error('2026-11-04T10:00:00Z', 'getaddrinfo EAI_AGAIN api.anthropic.com'))
  expect(open.stops).toEqual([{ fromMs: Date.parse('2026-11-04T10:00:00Z'), reason: 'no network' }])
  const closed = foldTranscript([error('2026-11-04T10:00:00Z', 'x'), error('2026-11-04T10:01:00Z', 'y'), turn('2026-11-04T10:05:00Z', { model: 'm', content: [] })].join('\n'))
  expect(closed.stops).toEqual([{ fromMs: Date.parse('2026-11-04T10:00:00Z'), toMs: Date.parse('2026-11-04T10:05:00Z'), reason: 'API error' }])
  const words = (error: string | undefined, text: string) => stopReason({ error, message: { content: [{ text }] } })
  expect([words('rate_limit', ''), words('authentication_failed', ''), words(undefined, 'Request timed out'), words(undefined, 'hit your limit'), words(undefined, 'Please run /login'), words(undefined, 'boom')]).toEqual([
    'usage limit',
    'login expired',
    'timed out',
    'usage limit',
    'login expired',
    'API error',
  ])
  // An older span past the tail stays; one still open takes the end a later read found.
  expect(mergeStops([{ fromMs: 1, toMs: 2, reason: 'a' }, { fromMs: 5, reason: 'b' }], [{ fromMs: 5, toMs: 9, reason: 'b' }, { fromMs: 20, reason: 'c' }])).toEqual([
    { fromMs: 1, toMs: 2, reason: 'a' },
    { fromMs: 5, toMs: 9, reason: 'b' },
    { fromMs: 20, reason: 'c' },
  ])
})

test('the files beside the feeds read forgivingly', async () => {
  const etas = parseEtas(JSON.stringify({ agents: { a: { left_s: 480, at: '2026-10-06T10:06:30Z', first_left_s: 300, first_at: '2026-10-06T10:00:00Z' }, bad: { left_s: 'x' } } }))
  expect(etas).toEqual({ a: { leftS: 480, atMs: Date.parse('2026-10-06T10:06:30Z'), firstLeftS: 300, firstAtMs: Date.parse('2026-10-06T10:00:00Z') } })
  expect(parseEtas('torn')).toEqual({})
  expect(parseSleeps('{"since":1,"until":2}\n\ntorn\n{"since":3,"until":4}\n')).toEqual([
    { since: 1, until: 2 },
    { since: 3, until: 4 },
  ])
  expect(parseShells(JSON.stringify({ shells: [{ agent: 'w', cmd: 'sleep 40', id: 'b', started_ms: 7, task: 't' }, { agent: 'w' }] }))).toEqual([{ agent: 'w', shell: { id: 'b', cmd: 'sleep 40', startedMs: 7 } }])
  const toml = (body: string) => `[other]\ndone_rows = "hide"\n\n[management_panel]\n${body}\n\n[management_panel.columns]\nstage = true\n`
  expect(parseDoneRows(toml('done_rows = "all"\nask_names = true'))).toBe('all')
  expect(parseDoneRows(toml('done_rows = "hide"'))).toBe('hide')
  expect(parseDoneRows(toml('done_rows = "none"'))).toBe('hide')
  expect(parseDoneRows(toml('done_rows = "last"\ndone_last = 7'))).toBe(7)
  expect(parseDoneRows(toml('done_rows = "last"'))).toBe(5)
  expect(parseDoneRows(toml('done_rows = "odd"'))).toBe('all')
  expect(parseDoneRows('no such section')).toBe('all')
})
