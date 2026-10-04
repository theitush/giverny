// strip-spike (giverny#186): keep Claude Code's NATIVE subagent strip as the
// agents panel. The mod runs a tiny pass (a queue, K workers at a time), keeps
// each worker's data (name, stage, start, ETA, tokens) in ../state/rows.json,
// and bin/strip-rows.py (the subagentStatusLine script) prints our text into
// each native row. Next up and Done, which the strip cannot hold, go in an
// AbovePrompt band. Observations go to ../logs/mod.jsonl.
import type { Register, EngineInterface } from 'claude-code'

const HERE = '/home/ita/giverny/.claude/worktrees/task-186-mod-spike/mods-spike/strip'
const ROWS = `${HERE}/state/rows.json`
const WORKER = 'strip-spike:worker'

async function log($: EngineInterface, kind: string, data: Record<string, unknown> = {}) {
  const line = JSON.stringify({ t: await $.clock.now(), kind, ...data }) + '\n'
  await $.process.run(['tee', '-a', `${HERE}/logs/mod.jsonl`], { stdin: line })
}

type Task = { name: string; secs: number; perm?: boolean }
type Row = { name: string; stage: string; startedMs: number; etaMs: number; tokens: number; status: 'running' | 'done' | 'error'; endedMs?: number }
const queue: Task[] = []
const rows = new Map<string, Row>()
let width = 3

const dur = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000))
  return s >= 60 ? `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s` : `${s}s`
}

async function save($: EngineInterface, why: string) {
  const t = await $.clock.now()
  await $.fs.write(ROWS, JSON.stringify({ updatedMs: t, why, rows: Object.fromEntries(rows), queue: queue.map(q => ({ name: q.name, etaMs: (q.secs + 20) * 1000 })) }, null, 1))
  $.ui.invalidate('ui.render')
}

const STAGES = ['reading', 'coding', 'testing']
const WORKER_PROMPT =
  'You are a test worker. Run each Bash command the task gives you, one at a time, in the foreground (never run_in_background), exactly as written. Then reply with the one line it asks for.'

async function spawnNext($: EngineInterface) {
  while (queue.length && [...rows.values()].filter(r => r.status === 'running').length < width) {
    const task = queue.shift()!
    const per = Math.round(task.secs / STAGES.length)
    const cmds = STAGES.map(s => `echo ${s}; sleep ${per}`)
    if (task.perm) cmds.splice(1, 0, 'touch perm-probe.txt')
    const prompt = `Run these Bash commands one after another, each as its own Bash call:\n${cmds.map((c, i) => `${i + 1}. ${c}`).join('\n')}\nThen reply "DONE ${task.name}".`
    const t = await $.clock.now()
    const r = await $.agent.spawn({ subagentType: WORKER, prompt, description: task.name, model: 'haiku' })
    await log($, 'spawn', { name: task.name, r })
    if (r.agentId) rows.set(r.agentId, { name: task.name, stage: 'starting', startedMs: t, etaMs: t + (task.secs + 20) * 1000, tokens: 0, status: 'running' })
    await save($, `spawn ${task.name}`)
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const out = await next(e)
    await $.agent
      .register({ name: 'worker', description: 'strip-spike test worker (spawned by the mod only)', prompt: WORKER_PROMPT, tools: ['Bash'], model: 'haiku', omitClaudeMd: true })
      .then(r => log($, 'agent.register', { r }), err => log($, 'agent.register.error', { err: String(err) }))
    await $.command.register({ name: 'ss-pass', description: 'Run a pass: N tasks, K at a time', argumentHint: '<n> <k> [secs]', immediate: true })
    await $.command.register({ name: 'ss-nudge', description: 'Try to make Claude Code re-run the subagentStatusLine script now', argumentHint: 'invalidate|touch', immediate: true })
    await $.command.register({ name: 'ss-mode', description: 'Set the strip script test mode', argumentHint: '<mode>', immediate: true })
    await $.command.register({ name: 'ss-install', description: "Write subagentStatusLine into this project's .claude/settings.local.json", immediate: true })
    await log($, 'session.start', { cwd: await $.session.cwd() })
    return out
  })

  on('agent.offer', { agent: WORKER }, () => ({ isOffered: false }))

  on('command.run', { command: 'ss-pass' }, async ($, e) => {
    const [n, k, secs] = (e.args || '').split(/\s+/).map(Number)
    width = k || 3
    const names = ['parser', 'renderer', 'ledger', 'eta-model', 'band', 'inbox', 'limits', 'brief', 'land']
    for (let i = 0; i < (n || 5); i++) queue.push({ name: `t${i + 1}-${names[i % names.length]}`, secs: (secs || 90) + i * 15, perm: names[i % names.length] === 'ledger' })
    await spawnNext($)
    return { text: `queued ${n || 5}, width ${width}` }
  })

  on('command.run', { command: 'ss-mode' }, async ($, e) => {
    await $.fs.write(`${HERE}/state/mode`, (e.args || 'ours').trim())
    await log($, 'mode', { mode: e.args })
    return { text: `mode ${e.args}` }
  })

  on('command.run', { command: 'ss-nudge' }, async ($, e) => {
    const how = (e.args || 'invalidate').trim()
    if (how === 'touch') {
      const f = `${await $.session.cwd()}/.claude/settings.local.json`
      await $.fs.write(f, await $.fs.read(f))
    } else {
      $.ui.invalidate('ui.render')
    }
    await log($, 'nudge', { how })
    return { text: `nudged (${how})` }
  })

  on('command.run', { command: 'ss-install' }, async $ => {
    const f = `${await $.session.cwd()}/.claude/settings.local.json`
    let cur: Record<string, unknown> = {}
    if (await $.fs.exists(f)) cur = JSON.parse(await $.fs.read(f))
    cur.subagentStatusLine = { type: 'command', command: `${HERE}/bin/strip-rows.py` }
    await $.fs.write(f, JSON.stringify(cur, null, 2) + '\n')
    await log($, 'install', { f })
    return { text: `wrote subagentStatusLine into ${f}` }
  })

  on('tool.call', async ($, e, next) => {
    const r = e.agentId ? rows.get(e.agentId) : undefined
    if (r) {
      const cmd = String((e as unknown as { command?: string }).command ?? '')
      const m = /^echo (\w+);/.exec(cmd)
      r.stage = m ? m[1] : cmd.startsWith('touch') ? 'waiting: permission' : r.stage
      await save($, `tool ${r.name} ${r.stage}`)
    }
    return next(e)
  })

  on('turn.step', async function* ($, e, next) {
    const r = e.agentId ? rows.get(e.agentId) : undefined
    const res = yield* next(e)
    if (r && res.usage) {
      const u = res.usage as { input_tokens?: number; output_tokens?: number; cache_read_input_tokens?: number; cache_creation_input_tokens?: number }
      r.tokens += (u.input_tokens ?? 0) + (u.output_tokens ?? 0) + (u.cache_creation_input_tokens ?? 0)
      await save($, `step ${r.name}`)
    }
    return res
  })

  on('turn.complete', async ($, e, next) => {
    const out = await next(e)
    const r = e.agentId ? rows.get(e.agentId) : undefined
    if (r) {
      r.status = e.reason === 'answer' ? 'done' : 'error'
      r.endedMs = await $.clock.now()
      r.stage = 'done'
      await save($, `complete ${r.name}`)
      await log($, 'complete', { agentId: e.agentId, name: r.name, reason: e.reason })
      await spawnNext($)
    }
    return out
  })

  // Next up and Done, above the prompt; the Running rows are the native strip below it.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (queue.length === 0 && rows.size === 0) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    const t = await $.clock.now()
    const done = [...rows.values()].filter(r => r.status !== 'running')
    const running = [...rows.values()].filter(r => r.status === 'running').length
    // mode "tail": Next up and Done ride under the strip's last row instead, so
    // the band only stands in while no worker runs (the strip is gone then).
    const mode = (await $.fs.read(`${HERE}/state/mode`).catch(() => '')).trim()
    if (mode === 'tail' && running > 0) return next(e)
    return (
      <Box flexDirection="column">
        <Box>
          <Text bold>Pass</Text>
          <Text dimColor> · {running} running ↓ in the strip · {queue.length} next · {done.length} done</Text>
        </Box>
        {queue.slice(0, 3).map((q, i) => (
          <Text key={`q:${i}`} wrap="truncate-end">
            <Text color="#5f87af">  ◌ next </Text>
            <Text>{q.name}</Text>
            <Text dimColor> ~{dur((q.secs + 20) * 1000)}</Text>
          </Text>
        ))}
        {queue.length > 3 && <Text dimColor>    +{queue.length - 3} more queued</Text>}
        {done.slice(-3).map(r => (
          <Text key={`d:${r.name}`} wrap="truncate-end">
            <Text color="#008700">  ✓ done </Text>
            <Text>{r.name}</Text>
            <Text dimColor> took {dur((r.endedMs ?? t) - r.startedMs)} · {(r.tokens / 1000).toFixed(1)}k tok</Text>
          </Text>
        ))}
      </Box>
    )
  })
}
