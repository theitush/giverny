import type { EngineInterface } from 'claude-code'
import { expect, test } from 'claude-code/testing'

import type { AgentRow, FeedRow } from '../types'

import { buildModel } from './model'
import { cpuPct, loadOf, memShare, newUseWatch, parsePass, parsePss, parseStat, pidTicks, readUse, seenOf, subtree, workerOf, wrappedCommand } from './stats'
import { foldTranscript } from './transcript'

const stat = (pid: number, ppid: number, ticks: number, start = 1000, name = 'x') =>
  `${pid} (${name}) S ${ppid} 1 1 0 -1 4194304 0 0 0 0 ${ticks} 0 0 0 20 0 1 0 ${start} 100 10 0`
const NOW = Date.parse('2026-11-04T10:00:00Z')

test('a stat line reads whatever its name holds, and a torn one reads as nothing', async () => {
  expect(parseStat(stat(7, 1, 30, 5, 'tmux: (a) b'))).toEqual({ pid: 7, ppid: 1, ticks: 30, start: 5 })
  expect(parseStat('7 (x')).toBe(undefined)
  expect(parseStat('7 (x) S')).toBe(undefined)
})

test('a pass is the table, then each cgroup’s pids', async () => {
  const { procs, cgroups } = parsePass(`${stat(1, 0, 5)}\n${stat(2, 1, 5)}\n@/cg/a/cgroup.procs\n2\n3\n@/cg/b/cgroup.procs\n`)
  expect(procs.map(p => p.pid)).toEqual([1, 2])
  expect(cgroups.get('/cg/a/cgroup.procs')).toEqual([2, 3])
  expect(cgroups.get('/cg/b/cgroup.procs')).toEqual([])
})

test('ticks since the last pass: one that ended counts its last stretch once, on whoever reaped it', async () => {
  const procs = (lines: string[]) => lines.map(l => parseStat(l)!)
  const then = procs([stat(1, 0, 100), stat(2, 1, 50), stat(3, 2, 40)])
  expect([...pidTicks(then, undefined).values()]).toEqual([0, 0, 0])
  // 3 ended: its 45 ticks went into 2, which spent 5 of its own; 40 of them were counted last time.
  const now = procs([stat(1, 0, 100), stat(2, 1, 100), stat(4, 2, 7)])
  const spent = pidTicks(now, seenOf(then))
  expect([spent.get(1), spent.get(2), spent.get(4)]).toEqual([0, 10, 7])
  // A pid back with another start is another process.
  expect(pidTicks(procs([stat(3, 2, 3, 2000)]), seenOf(then)).get(3)).toBe(3)
  expect([...subtree(now, 2)].sort()).toEqual([2, 4])
})

test('a figure is a share of the whole machine, and its colour goes by that share', async () => {
  // 200 ticks are 2 s of CPU: in 4 s on 2 cores, a quarter of the machine.
  expect(cpuPct(200, 4000, 2)).toBe(25)
  expect(cpuPct(200, 0, 2)).toBe(0)
  expect(cpuPct(100_000, 4000, 2)).toBe(100)
  expect([loadOf(19), loadOf(20), loadOf(50), loadOf(51)]).toEqual(['light', 'busy', 'busy', 'heavy'])
  expect(memShare(1024, 4096)).toBe(25)
  expect(memShare(1024, undefined)).toBe(0)
  expect(parsePss('/proc/12/smaps_rollup:Pss:     2048 kB\n/proc/13/smaps_rollup:Pss: 7 kB\n')).toEqual(new Map([[12, 2048], [13, 7]]))
})

test('a Bash wrapper names its command, and only one worker with it in flight owns it', async () => {
  const wrapper = (word: string) => `/bin/bash\0-c\0source /tmp/snap && shopt -u extglob && eval ${word} < /dev/null && pwd -P >| /tmp/cwd\0`
  expect(wrappedCommand(wrapper(`'cargo test -p a'`))).toBe('cargo test -p a')
  expect(wrappedCommand(wrapper(`'it'\\''s' `))).toBe(`it's`)
  expect(wrappedCommand(wrapper(`"a \\"b\\" \\$c"`))).toBe('a "b" $c')
  expect(wrappedCommand(`/usr/bin/node\0-c\0eval 'x' < /dev/null\0`)).toBe(undefined)
  expect(wrappedCommand(`/bin/bash\0-c\0echo hi\0`)).toBe(undefined)
  const calls = new Map([['w1', ['make', 'ls']], ['w2', ['ls']]])
  expect(workerOf('make', calls)).toBe('w1')
  expect(workerOf('ls', calls)).toBe(undefined)
  expect(workerOf('pwd', calls)).toBe(undefined)
})

test('a transcript says which Bash commands are in flight', async () => {
  const assistant = (content: unknown[]) => JSON.stringify({ type: 'assistant', timestamp: '2026-11-04T10:00:00Z', message: { model: 'm', content } })
  const bash = (id: string, command: string, run_in_background?: boolean) => ({ type: 'tool_use', id, name: 'Bash', input: { command, run_in_background } })
  const result = (id: string) => JSON.stringify({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: id, content: 'ok' }] } })
  const text = [assistant([bash('a', 'done')]), result('a'), assistant([bash('b', 'server', true)]), result('b'), assistant([bash('c', 'cargo test')])].join('\n')
  expect(foldTranscript(text).commands).toEqual(['server', 'cargo test'])
})

test('a row’s use: its worker and its runs once each, a Done row’s peak alone, nothing on Next up', async () => {
  const rows: FeedRow[] = [
    { key: 'a', stage: 'running', title: 'a', agent_id: 'w1' },
    { key: 'b', stage: 'running', title: 'b', agent_id: 'w2' },
    { key: 'c', stage: 'running', title: 'c', agent_id: 'w3' },
    { key: 'p', stage: 'planned', title: 'p', eta_s: 60 },
    { key: 'd', stage: 'done', title: 'd', agent_id: 'w4', usage: { peak_mb: 2150 } },
  ]
  const agent = (id: string, status = 'running'): AgentRow => ({ id, description: id, status })
  const use = {
    workers: { w1: { cpuPct: 30, memMb: 1000 }, w2: { cpuPct: 5, memMb: 100 }, w3: { cpuPct: 0, memMb: 0 } },
    // a's run is under its own worker, whose figure holds it; b's was started from elsewhere.
    tasks: { a: { use: { cpuPct: 20, memMb: 800 }, agent: 'w1' }, b: { use: { cpuPct: 60, memMb: 3000 } } },
    ramMb: 4000,
  }
  const model = buildModel({ aliases: [], rows }, ['w1', 'w2', 'w3'].map(i => agent(i)).concat(agent('w4', 'completed')), NOW, { manager: true, use })
  const of = (key: string) => model.rows.find(r => r.key === key)?.use
  expect(of('a')).toEqual({ cpuPct: 30, memMb: 1000, cpuLoad: 'busy', memLoad: 'busy' })
  expect(of('b')).toEqual({ cpuPct: 65, memMb: 3100, cpuLoad: 'heavy', memLoad: 'heavy' })
  expect(of('c')).toBe(undefined)
  expect(of('p')).toBe(undefined)
  expect(of('d')).toEqual({ memMb: 2150, cpuLoad: 'light', memLoad: 'light' })
})

test('a pass measures a worker’s command and a run’s scope, and with nothing going it runs nothing', async () => {
  let table = ''
  const asked: string[][] = []
  const files: Record<string, string> = {
    '/proc/self/stat': stat(10, 1, 0),
    '/proc/meminfo': 'MemTotal: 8388608 kB\n',
    '/proc/20/cmdline': `/bin/bash\0-c\0source x && eval 'cargo test' < /dev/null && pwd -P\0`,
    '/runs/1.stats': 'cgroup /cg/scope\n',
  }
  // A test's own `$` holds only the engine's nouns: this stands in for the two a pass uses.
  const $ = {
    fs: {
      read: async (path: string) => {
        if (files[path] === undefined) throw new Error('missing')
        return files[path]
      },
    },
    process: {
      run: async (argv: string[]) => {
        asked.push(argv)
        const stdout = argv[0] === 'nproc' ? '2\n' : argv.join(' ').includes('grep') ? '/proc/20/smaps_rollup:Pss: 1024 kB\n/proc/21/smaps_rollup:Pss: 2048 kB\n/proc/30/smaps_rollup:Pss: 4096 kB\n' : table
        return { exitCode: 0, stdout, stderr: '' }
      },
    },
  } as unknown as EngineInterface
  const w = newUseWatch()
  const calls = new Map([['w1', ['cargo test']]])
  const runs = [{ task: 'build', session: 's', pid: 99, stats: '/runs/1.stats' }]
  const at = (ticks: number) => `${stat(10, 1, 0)}\n${stat(20, 10, ticks)}\n${stat(21, 20, ticks)}\n${stat(30, 1, ticks)}\n@/cg/scope/cgroup.procs\n30\n`

  table = at(0)
  const first = await readUse($, w, runs, calls, true, NOW)
  // With no last pass there is no CPU yet; the memory is known.
  expect(first.workers.w1).toEqual({ cpuPct: 0, memMb: 3 })
  expect(first.tasks.build).toEqual({ use: { cpuPct: 0, memMb: 4 }, agent: undefined })
  expect(first.ramMb).toBe(8192)

  // Sooner than a pass is due: the last figures, and no child.
  const spawned = asked.length
  expect(await readUse($, w, runs, calls, true, NOW + 1000)).toBe(first)
  expect(asked.length).toBe(spawned)

  // 4 s on 2 cores are 800 ticks: 200 of them each is 25% of the machine, two processes 50%.
  table = at(200)
  const second = await readUse($, w, runs, calls, true, NOW + 4000)
  expect(second.workers.w1?.cpuPct).toBe(50)
  expect(second.tasks.build?.use.cpuPct).toBe(25)
  // The one pass is at most a `cat` and a `grep`.
  expect(asked.length - spawned).toBe(2)

  const idle = asked.length
  expect(await readUse($, w, [], new Map(), false, NOW + 8000)).toEqual({ tasks: {}, workers: {} })
  expect(asked.length).toBe(idle)
})
