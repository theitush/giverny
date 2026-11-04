import type { EngineInterface } from 'claude-code'

import type { Load, RunFile, Use, UseReading } from '../types'

// What a task's commands use right now: the band's use column. crates/claude/src/use_reading.rs,
// worker_pids.rs and session_use.rs, for one claude rather than a whole app, with the plugin's own
// cost held down (giverny#43). A call on `$` costs about 5 ms and a `cat` of every process's stat
// about the same as six of them, so one pass is one or two children, never a read per process:
// every process's `stat` and the run scopes' `cgroup.procs` from one `cat`, the proportional sets
// of the processes on show from one `grep`. No running worker and no run: no pass at all.

/** Passes are this far apart; the app's is one a second, and a figure a person reads needs no more. */
const USE_READ_MS = 4000
/** How many passes a command that matches no worker is tried again: its transcript is re-read every 6 s. */
const TRIES = 4
/** USER_HZ, which Linux keeps at 100 for /proc whatever the kernel's own tick. */
const TICKS_PER_S = 100

export type Proc = { pid: number; ppid: number; ticks: number; start: number }
/** A process at the last pass: its parent, its start and its ticks. */
export type Seen = Map<number, Proc>

export type UseWatch = {
  prev?: { seen: Seen; atMs: number }
  /** This claude, the machine's cores and RAM: read once. */
  claude?: number
  cores?: number
  ramMb?: number
  /** `pid:start` to its worker, or null once given up on: a pid's match never changes. */
  known: Map<string, string | null>
  tries: Map<string, number>
  /** A run's stats file to its scope's cgroup, once the shim has said it. */
  cgroups: Map<string, string>
  readAtMs: number
  last: UseReading
}

export const NO_USE: UseReading = { tasks: {}, workers: {} }

export const newUseWatch = (): UseWatch => ({ known: new Map(), tries: new Map(), cgroups: new Map(), readAtMs: 0, last: NO_USE })

/** One `/proc/<pid>/stat` line. The name, in parentheses, may hold spaces and parentheses itself. */
export function parseStat(line: string): Proc | undefined {
  const close = line.lastIndexOf(')')
  if (close < 0) return undefined
  const pid = Number.parseInt(line, 10)
  // From `state`, field 3: ppid is field 4, utime to cstime 14-17, starttime 22.
  const f = line.slice(close + 2).split(' ')
  const n = (field: number) => Number(f[field - 3])
  const ticks = n(14) + n(15) + n(16) + n(17)
  if (!Number.isFinite(pid) || !Number.isFinite(ticks) || !Number.isFinite(n(4)) || !Number.isFinite(n(22))) return undefined
  return { pid, ppid: n(4), ticks, start: n(22) }
}

/** `root` and everything below it. */
export function subtree(procs: Proc[], root: number): Set<number> {
  const children = new Map<number, number[]>()
  for (const p of procs) {
    if (p.pid === p.ppid) continue
    const siblings = children.get(p.ppid)
    if (siblings) siblings.push(p.pid)
    else children.set(p.ppid, [p.pid])
  }
  const alive = new Set(procs.map(p => p.pid))
  const out = new Set<number>()
  const stack = [root]
  while (stack.length > 0) {
    const pid = stack.pop()!
    if (out.has(pid) || !alive.has(pid)) continue
    out.add(pid)
    stack.push(...(children.get(pid) ?? []))
  }
  return out
}

export const seenOf = (procs: Proc[]): Seen => new Map(procs.map(p => [p.pid, p]))

/**
 * Each live process's ticks since the last pass. One seen then spent the difference; one new since
 * spent all it has. One that ended since had its time up to the last pass added into whichever
 * process reaped it, so that much comes back off its nearest ancestor still alive: only its last
 * stretch counts, once. With no last pass nothing is known yet.
 */
export function pidTicks(procs: Proc[], prev: Seen | undefined): Map<number, number> {
  if (!prev) return new Map(procs.map(p => [p.pid, 0]))
  const alive = new Map(procs.map(p => [p.pid, p.start]))
  const back = new Map<number, number>()
  for (const [pid, then] of prev) {
    if (alive.get(pid) === then.start) continue
    let up = then.ppid
    for (let i = 0; i < 64; i++) {
      if (alive.has(up)) {
        back.set(up, (back.get(up) ?? 0) + then.ticks)
        break
      }
      const above = prev.get(up)
      if (!above || above.ppid === up) break
      up = above.ppid
    }
  }
  return new Map(
    procs.map(p => {
      const then = prev.get(p.pid)
      const spent = then && then.start === p.start ? p.ticks - then.ticks : p.ticks
      return [p.pid, Math.max(0, spent - (back.get(p.pid) ?? 0))]
    }),
  )
}

/** One shell word from the start of `s`, and what follows it; undefined for an unterminated quote. */
function shellWord(s: string): [string, string] | undefined {
  let out = ''
  let i = 0
  while (i < s.length) {
    const c = s[i]!
    if (c === "'") {
      const end = s.indexOf("'", i + 1)
      if (end < 0) return undefined
      out += s.slice(i + 1, end)
      i = end + 1
    } else if (c === '"') {
      i++
      for (;;) {
        if (i >= s.length) return undefined
        const d = s[i]!
        if (d === '"') break
        if (d === '\\') {
          const e = s[i + 1]
          if (e === undefined) return undefined
          if (e === '"' || e === '\\' || e === '$' || e === '`') out += e
          else if (e !== '\n') out += `\\${e}`
          i += 2
        } else {
          out += d
          i++
        }
      }
      i++
    } else if (c === '\\') {
      out += s[i + 1] ?? ''
      i += 2
    } else if (/\s/.test(c)) {
      return [out, s.slice(i)]
    } else {
      out += c
      i++
    }
  }
  return [out, '']
}

/** The command Claude Code's Bash wrapper evals, out of a `/proc/<pid>/cmdline`; undefined when it is no wrapper. */
export function wrappedCommand(cmdline: string): string | undefined {
  const [bin, flag, script] = cmdline.split('\0')
  if (!bin || flag !== '-c' || !script || !/(^|\/)(ba|z)?sh$/.test(bin)) return undefined
  const at = script.indexOf('&& eval ')
  if (at < 0) return undefined
  const word = shellWord(script.slice(at + '&& eval '.length))
  return word && word[1].trimStart().startsWith('< /dev/null') ? word[0] : undefined
}

/** The one worker with `command` in flight; undefined with none or two. Commands of the session's own count as none. */
export function workerOf(command: string, calls: Map<string, string[]>): string | undefined {
  const who = [...calls].filter(([, commands]) => commands.includes(command)).map(([id]) => id)
  return who.length === 1 ? who[0] : undefined
}

/** `grep -H ^Pss:` over smaps_rollup files: KiB by pid. */
export function parsePss(text: string): Map<number, number> {
  const out = new Map<number, number>()
  for (const m of text.matchAll(/^\/proc\/(\d+)\/smaps_rollup:Pss:\s+(\d+)/gm)) out.set(Number(m[1]), Number(m[2]))
  return out
}

/** How much of the whole machine `ticks` over `overMs` is, every core busy being 100. */
export function cpuPct(ticks: number, overMs: number, cores: number): number {
  if (overMs <= 0 || cores <= 0) return 0
  return Math.min(100, Math.max(0, Math.round((ticks * 1000 * 100) / TICKS_PER_S / (overMs * cores))))
}

/** How heavy a share of the machine is, as every use figure is coloured: `sessions_load.rs`. */
export const loadOf = (share: number): Load => (share > 50 ? 'heavy' : share >= 20 ? 'busy' : 'light')

/** The whole machine's share a row's memory is, with no RAM known never high. */
export const memShare = (mb: number, ramMb: number | undefined) => (ramMb ? (mb * 100) / ramMb : 0)

/** Two disjoint sets of processes' use together. */
export const plusUse = (a: Use, b: Use): Use => ({ cpuPct: Math.min(100, a.cpuPct + b.cpuPct), memMb: a.memMb + b.memMb })

/** Output of one pass's `cat`: stat lines, and after each `@<path>` line that cgroup's pids. */
export function parsePass(text: string): { procs: Proc[]; cgroups: Map<string, number[]> } {
  const procs: Proc[] = []
  const cgroups = new Map<string, number[]>()
  let group: number[] | undefined
  for (const line of text.split('\n')) {
    if (line.startsWith('@')) {
      group = []
      cgroups.set(line.slice(1), group)
    } else if (group) {
      if (/^\d+$/.test(line)) group.push(Number(line))
    } else if (line !== '') {
      const p = parseStat(line)
      if (p) procs.push(p)
    }
  }
  return { procs, cgroups }
}

/** The worker of each child of this claude that is a Bash wrapper, each asked once. */
async function attribute($: EngineInterface, w: UseWatch, procs: Proc[], calls: Map<string, string[]>): Promise<Map<string, Set<number>>> {
  const out = new Map<string, Set<number>>()
  const alive = new Set(procs.map(p => `${p.pid}:${p.start}`))
  for (const key of w.known.keys()) if (!alive.has(key)) w.known.delete(key)
  for (const key of w.tries.keys()) if (!alive.has(key)) w.tries.delete(key)
  for (const p of procs) {
    if (p.ppid !== w.claude) continue
    const key = `${p.pid}:${p.start}`
    let worker = w.known.get(key)
    if (worker === undefined) {
      let command: string | undefined
      try {
        command = wrappedCommand(await $.fs.read(`/proc/${p.pid}/cmdline`))
      } catch {
        continue
      }
      const found = command === undefined ? undefined : workerOf(command, calls)
      const tries = (w.tries.get(key) ?? 0) + 1
      w.tries.set(key, tries)
      if (found) w.known.set(key, found)
      else if (command === undefined || tries >= TRIES) w.known.set(key, null)
      worker = found ?? null
    }
    if (!worker) continue
    const mine = out.get(worker) ?? new Set<number>()
    for (const pid of subtree(procs, p.pid)) mine.add(pid)
    out.set(worker, mine)
  }
  return out
}

async function readOnce($: EngineInterface, w: UseWatch): Promise<void> {
  w.claude ??= Number.parseInt(await $.fs.read('/proc/self/stat'), 10)
  w.cores ??= Number((await $.process.run(['nproc'])).stdout.trim()) || 1
  if (w.ramMb === undefined) {
    const kb = /^MemTotal:\s+(\d+)/m.exec(await $.fs.read('/proc/meminfo'))?.[1]
    w.ramMb = kb === undefined ? 0 : Math.ceil(Number(kb) / 1024)
  }
}

/** The scope of each run whose shim has said where it is. */
async function scopesOf($: EngineInterface, w: UseWatch, runs: RunFile[]): Promise<void> {
  for (const r of runs) {
    if (w.cgroups.has(r.stats)) continue
    try {
      const dir = /^cgroup (.+)$/m.exec(await $.fs.read(r.stats))?.[1]?.trim()
      if (dir) w.cgroups.set(r.stats, dir)
    } catch {
      // Not written yet, or gone: the next pass asks again.
    }
  }
}

/**
 * One reading, at most every USE_READ_MS. `runs` are this session's going `giverny-manage run`s and
 * `calls` each worker's Bash commands in flight. A figure that cannot be read costs that figure.
 */
export async function readUse($: EngineInterface, w: UseWatch, runs: RunFile[], calls: Map<string, string[]>, anyRunning: boolean, now: number): Promise<UseReading> {
  if (!anyRunning && runs.length === 0) {
    w.prev = undefined
    w.last = NO_USE
    return NO_USE
  }
  if (now - w.readAtMs < USE_READ_MS - 200) return w.last
  w.readAtMs = now
  try {
    await readOnce($, w)
    await scopesOf($, w, runs)
    const paths = runs.flatMap(r => {
      const dir = w.cgroups.get(r.stats)
      return dir ? [`${dir}/cgroup.procs`] : []
    })
    const pass = await $.process.run(['sh', '-c', 'cat /proc/[0-9]*/stat 2>/dev/null; for f do echo "@$f"; cat "$f" 2>/dev/null; done', 'sh', ...paths])
    const { procs, cgroups } = parsePass(pass.stdout)
    const spent = pidTicks(procs, w.prev?.seen)
    const overMs = w.prev ? now - w.prev.atMs : 0
    w.prev = { seen: seenOf(procs), atMs: now }

    const workers = await attribute($, w, procs, calls)
    const runPids = new Map<string, Set<number>>()
    for (const r of runs) {
      const dir = w.cgroups.get(r.stats)
      const pids = dir ? (cgroups.get(`${dir}/cgroup.procs`) ?? []) : [...subtree(procs, r.pid)]
      const mine = runPids.get(r.task) ?? new Set<number>()
      for (const pid of pids) mine.add(pid)
      runPids.set(r.task, mine)
    }

    const shown = new Set([...workers.values(), ...runPids.values()].flatMap(s => [...s]))
    const pss = shown.size === 0 ? new Map<number, number>() : parsePss((await $.process.run(['sh', '-c', 'exec grep -H ^Pss: "$@" 2>/dev/null', 'sh', ...[...shown].map(p => `/proc/${p}/smaps_rollup`)])).stdout)
    const useOf = (pids: Set<number>): Use => {
      let ticks = 0
      let kb = 0
      for (const pid of pids) {
        ticks += spent.get(pid) ?? 0
        kb += pss.get(pid) ?? 0
      }
      return { cpuPct: cpuPct(ticks, overMs, w.cores ?? 1), memMb: Math.ceil(kb / 1024) }
    }
    const under = (pids: Set<number>) => [...workers].find(([, mine]) => [...pids].some(p => mine.has(p)))?.[0]
    w.last = {
      tasks: Object.fromEntries([...runPids].map(([task, pids]) => [task, { use: useOf(pids), agent: under(pids) }])),
      workers: Object.fromEntries([...workers].map(([id, pids]) => [id, useOf(pids)])),
      ramMb: w.ramMb || undefined,
    }
  } catch {
    // Keep the last figures; the next pass tries again.
  }
  return w.last
}
