// pass-spike: giverny#186 mechanics probes. Every observation is appended as one
// JSON line to ../../logs/<label>.jsonl (label = PASS_SPIKE_LABEL or the session id),
// so FINDINGS.md can cite the log rather than memory.
import type { Register, EngineInterface } from 'claude-code'

const LOG_DIR = '/home/ita/giverny/.claude/worktrees/task-186-mod-spike/mods-spike/mechanics/logs'
const LOAD_ID = Math.random().toString(36).slice(2, 8) // changes on every (re)load of the module

let label: string | undefined
async function log($: EngineInterface, kind: string, data: Record<string, unknown> = {}) {
  if (label === undefined) {
    const env = await $.process.run(['printenv', 'PASS_SPIKE_LABEL']).catch(() => undefined)
    label = env?.stdout.trim() || (await $.session.id())
  }
  const line = JSON.stringify({ t: await $.clock.now(), load: LOAD_ID, kind, ...data }) + '\n'
  await $.process.run(['tee', '-a', `${LOG_DIR}/${label}.jsonl`], { stdin: line })
}

const WORKER = 'pass-spike:worker'
const WORKER_PROMPT =
  'You are a test worker. Do exactly what the task says, using the Bash tool, then reply with the one word it asks for. Keep replies under ten words.'

// ---- the pass loop's state: module memory, mirrored to $.store so a reload can see it
type Task = { id: string; prompt: string; agentId?: string; status: 'queued' | 'running' | 'landed'; answer?: string }
let loop: { tasks: Task[]; slots: number } | undefined

async function saveLoop($: EngineInterface) {
  await $.store.set(`loop:${await $.session.id()}`, loop ?? null)
}

async function fillSlots($: EngineInterface) {
  if (!loop) return
  const running = loop.tasks.filter(t => t.status === 'running').length
  const free = loop.slots - running
  const next = loop.tasks.filter(t => t.status === 'queued').slice(0, Math.max(0, free))
  await Promise.all(
    next.map(async t => {
      const r = await $.agent.spawn({ subagentType: WORKER, prompt: t.prompt, description: `loop ${t.id}`, model: 'haiku' })
      t.agentId = r.agentId
      t.status = 'running'
      await log($, 'loop.spawn', { task: t.id, result: r })
    }),
  )
  await saveLoop($)
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const out = await next(e)
    await log($, 'session.start', { e })
    await $.agent.register({
      name: 'worker',
      description: 'pass-spike test worker (spawned by the mod only)',
      prompt: WORKER_PROMPT,
      tools: ['Bash'],
      model: 'haiku',
      omitClaudeMd: true,
    }).then(r => log($, 'agent.register', { r }), err => log($, 'agent.register.error', { err: String(err) }))
    for (const [name, description] of [
      ['spike-par', 'Spawn 3 workers at once, each sleeping 30 s'],
      ['spike-inbox', 'Spawn 1 worker, then append a message to it mid-run'],
      ['spike-loop', 'Run a 3-task pass loop in mod code, 2 slots'],
      ['spike-list', 'Log $.agent.list()'],
      ['spike-store', 'Store race: N read-modify-write increments of a shared key'],
      ['spike-keys', 'Store race: N writes of disjoint keys'],
      ['spike-usage', 'Log $.session.usage()'],
      ['spike-proc', 'Stream a systemd-run --scope child through $.process.spawn'],
      ['spike-timer', 'Start a 5 s heartbeat timer from a command'],
    ] as const) {
      await $.command.register({ name, description, immediate: true })
    }
    // a heartbeat started at session.start: shows whether timers survive /clear and reloads
    $.clock.every(10_000, () => void log($, 'heartbeat.start-timer'))
    return out
  })

  on('agent.offer', { agent: WORKER }, () => ({ isOffered: false }))

  // worker-side activity: one event per tool call, in any loop
  on('tool.call', async ($, e, next) => {
    const t0 = await $.clock.now()
    const desc = 'command' in e ? String((e as { command?: unknown }).command).slice(0, 80) : undefined
    await log($, 'tool.call.start', { agentId: e.agentId, tool: e.tool, desc })
    const r = await next(e)
    await log($, 'tool.call.end', { agentId: e.agentId, tool: e.tool, ms: (await $.clock.now()) - t0, isError: 'isError' in r ? r.isError : undefined })
    return r
  })

  on('turn.complete', async ($, e, next) => {
    await log($, 'turn.complete', { agentId: e.agentId, reason: e.reason, durationMs: e.durationMs, usage: e.usage, answer: e.answer.slice(0, 200) })
    const out = await next(e)
    const t = loop?.tasks.find(x => x.agentId !== undefined && x.agentId === e.agentId)
    if (loop && t) {
      t.status = 'landed'
      t.answer = e.answer
      // "land": a host command standing in for git merge
      const head = await $.process.run(['git', '-C', LOG_DIR, 'rev-parse', '--short', 'HEAD'])
      await log($, 'loop.land', { task: t.id, head: head.stdout.trim() })
      if (e.answer.includes('NEED-DECISION')) {
        await log($, 'loop.wake', { task: t.id })
        void $.prompt.submit({ text: `pass-spike: worker ${t.id} asks for a decision ("${e.answer.slice(0, 80)}"). Reply with just: GO.` } as never)
      }
      if (loop.tasks.every(x => x.status === 'landed')) {
        await log($, 'loop.done', { tasks: loop.tasks.map(x => ({ id: x.id, answer: x.answer })) })
        loop = undefined
        await saveLoop($)
      } else await fillSlots($)
    }
    return out
  })

  on('session.measure', async ($, e, next) => {
    await log($, 'session.measure', { rateLimits: e.rateLimits, changed: e.changed, cost: e.cost })
    return next(e)
  })

  on('session.end', async ($, e, next) => {
    await log($, 'session.end', { reason: e.reason, agents: await $.agent.list() })
    return next(e)
  })

  on('command.run', { command: 'spike-par' }, async ($, e) => {
    const t0 = await $.clock.now()
    const results = await Promise.all(
      [1, 2, 3].map(i =>
        $.agent.spawn({
          subagentType: WORKER,
          prompt: `Run this exact Bash command: date +%s.%N; sleep 30; date +%s.%N  -- then reply DONE${i}.`,
          description: `par ${i}`,
          model: 'haiku',
        }),
      ),
    )
    await log($, 'par.spawned', { ms: (await $.clock.now()) - t0, results })
    await log($, 'agent.list', { list: await $.agent.list() })
    return { text: `spawned ${results.map(r => r.agentId ?? r.deny).join(', ')}` }
  })

  on('command.run', { command: 'spike-inbox' }, async ($, e) => {
    const r = await $.agent.spawn({
      subagentType: WORKER,
      prompt: 'Run `sleep 15` in Bash. Then run `sleep 15` in Bash again. Then reply DONE, followed by any extra instruction you were given meanwhile.',
      description: 'inbox',
      model: 'haiku',
    })
    await log($, 'inbox.spawned', { r })
    const delay = Number(e.args || 8) * 1000
    $.clock.after(delay, async () => {
      const res = await $.session
        .append({
          agentId: r.agentId,
          message: { type: 'user', content: [{ type: 'text', text: 'NEW INSTRUCTION from the dispatcher: before you reply, also run `echo INBOX-OK` in Bash, and end your reply with the word PINEAPPLE.' }] },
        })
        .catch(err => ({ error: String(err) }))
      await log($, 'inbox.appended', { agentId: r.agentId, res })
    })
    return { text: `inbox worker ${r.agentId}; message in ${delay} ms` }
  })

  on('command.run', { command: 'spike-loop' }, async $ => {
    loop = {
      slots: 2,
      tasks: [
        { id: 't1', status: 'queued', prompt: 'Run `sleep 15` in Bash, then reply DONE-t1.' },
        { id: 't2', status: 'queued', prompt: 'Run `sleep 20` in Bash, then reply DONE-t2.' },
        { id: 't3', status: 'queued', prompt: 'Run `sleep 5` in Bash, then reply NEED-DECISION-t3.' },
      ],
    }
    await log($, 'loop.start', {})
    await fillSlots($)
    return { text: 'loop started (mod-driven; the model is not involved)' }
  })

  on('command.run', { command: 'spike-list' }, async $ => {
    const list = await $.agent.list()
    await log($, 'agent.list', { list, storedLoop: await $.store.get(`loop:${await $.session.id()}`), moduleLoop: loop ?? null })
    return { text: JSON.stringify(list) }
  })

  on('command.run', { command: 'spike-store' }, async ($, e) => {
    const n = Number(e.args || 50)
    const sid = await $.session.id()
    let lastSeen = 0
    for (let i = 0; i < n; i++) {
      const v = Number((await $.store.get('race')) ?? 0)
      await $.store.set('race', v + 1)
      lastSeen = v + 1
    }
    const rows = (await $.store.get('rows')) as Record<string, unknown> | undefined
    await $.store.set('rows', { ...(rows ?? {}), [sid]: { at: await $.clock.now(), label } })
    await log($, 'store.race', { n, lastSeen, final: await $.store.get('race'), keys: await $.store.keys(), rows: await $.store.get('rows') })
    return { text: `race done: wrote ${n}, final ${await $.store.get('race')}` }
  })

  // disjoint keys from two sessions at once: does one session's write clobber the other's?
  on('command.run', { command: 'spike-keys' }, async ($, e) => {
    const n = Number(e.args || 100)
    for (let i = 0; i < n; i++) await $.store.set(`k:${label}:${i}`, i)
    const keys = await $.store.keys()
    await log($, 'store.keys', { n, mine: keys.filter(k => k.startsWith(`k:${label}:`)).length, all: keys.filter(k => k.startsWith('k:')).length })
    return { text: `wrote ${n}` }
  })

  on('command.run', { command: 'spike-usage' }, async $ => {
    const u = await $.session.usage()
    await log($, 'usage', { u })
    return { text: JSON.stringify(u.rateLimits) }
  })

  on('command.run', { command: 'spike-proc' }, async ($, e) => {
    const secs = Number(e.args || 3)
    const argv = ['systemd-run', '--user', '--scope', '--quiet', '-p', 'MemoryMax=200M', '-p', 'MemorySwapMax=0', '--',
      'sh', '-c', `for i in $(seq ${secs}); do echo tick $i; [ $i = 1 ] && cat /proc/self/cgroup; sleep 1; done; echo done-in-scope`]
    const pieces: unknown[] = []
    for await (const p of $.process.spawn({ argv })) pieces.push({ t: await $.clock.now(), ...p })
    await log($, 'proc', { argv, pieces })
    return { text: `${pieces.length} pieces` }
  })

  on('command.run', { command: 'spike-timer' }, async $ => {
    $.clock.every(5_000, () => void log($, 'heartbeat.cmd-timer'))
    return { text: 'timer started' }
  })
}
