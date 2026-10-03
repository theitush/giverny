// worker-view-spike (giverny#186): the closest a mod gets to "click a worker's
// row and be inside that worker". A list pane of the workers this mod spawned;
// pressing a row opens a worker pane that follows the worker's transcript live
// ($.session.messages + its tool.call / turn.step / turn.complete events), with
// an Input that sends to the worker through $.session.append (the inbox), a
// Stop button ($.turn.abort on the worker's turnId) and Esc / Back to return.
// Every observation goes as one JSON line to ../logs/<label>.jsonl.
import type { Register, EngineInterface, RenderInput } from 'claude-code'

type RenderArg = RenderInput

const LOG_DIR = '/home/ita/giverny/.claude/worktrees/task-186-mod-spike/mods-spike/worker-view/logs'
const LIST = 'wv'
const HERE = '/home/ita/giverny/.claude/worktrees/task-186-mod-spike/mods-spike/worker-view'
const WORKER = 'worker-view-spike:worker'

let label: string | undefined
async function log($: EngineInterface, kind: string, data: Record<string, unknown> = {}) {
  if (label === undefined) {
    const env = await $.process.run(['printenv', 'WV_LABEL']).catch(() => undefined)
    label = env?.stdout.trim() || 'wv'
  }
  const line = JSON.stringify({ t: await $.clock.now(), kind, ...data }) + '\n'
  await $.process.run(['tee', '-a', `${LOG_DIR}/${label}.jsonl`], { stdin: line })
}

// ---- module state ----
type Line = { t: number; kind: 'tool' | 'tool-end' | 'text' | 'step' | 'sent' | 'done' | 'note'; text: string }
type Worker = {
  agentId: string
  name: string
  status: 'running' | 'done' | 'aborted' | 'error'
  startedMs: number
  endedMs?: number
  answer?: string
  turnId?: string
  live: string // text streaming in the current step
  lines: Line[]
  lastEventMs: number
  tools: number
}
const workers = new Map<string, Worker>()
let openId: string | undefined
let bandMode = false
let note = ''

function push(w: Worker, l: Line) {
  w.lines.push(l)
  if (w.lines.length > 400) w.lines.splice(0, w.lines.length - 400)
  w.lastEventMs = l.t
}

function redraw($: EngineInterface) {
  $.ui.invalidate('ui.render')
}

const dur = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000))
  return s >= 60 ? `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s` : `${s}s`
}
const one = (s: string, n: number) => {
  const t = s.replace(/\s+/g, ' ').trim()
  return t.length > n ? t.slice(0, Math.max(0, n - 1)) + '…' : t
}
function inputSummary(input: Record<string, unknown> | undefined): string {
  if (!input) return ''
  for (const k of ['command', 'file_path', 'pattern', 'url', 'prompt']) if (typeof input[k] === 'string') return String(input[k])
  return JSON.stringify(input)
}

// The worker's command: no shell variables, so the permission checker can read it.
const TICKS = Array.from({ length: 8 }, (_, i) => `echo tick ${i + 1}; sleep 6`).join('; ')
const WORKER_PROMPT =
  'You are a test worker. Do exactly what the task says, using the Bash tool in the foreground (never run_in_background), then reply with the one line it asks for. If a user message arrives while you work, do what it says too, and mention it in your reply.'

async function spawnOne($: EngineInterface, name: string) {
  const r = await $.agent.spawn({
    subagentType: WORKER,
    prompt: `Run this exact Bash command: ${TICKS}\nThen run it once more. Then reply "DONE ${name}" and a one-line summary.`,
    description: name,
    model: 'haiku',
  })
  await log($, 'spawn', { name, r })
  if (r.agentId) {
    const t = await $.clock.now()
    workers.set(r.agentId, { agentId: r.agentId, name, status: 'running', startedMs: t, live: '', lines: [], lastEventMs: t, tools: 0 })
  }
  return r
}

async function openWorker($: EngineInterface, id: string) {
  openId = id
  const t0 = await $.clock.now()
  const r = await $.ui.open({ id: LIST, title: `Worker ${workers.get(id)?.name ?? id}`, focus: true, closeOnEscape: true })
  await log($, 'open.worker', { id, ms: (await $.clock.now()) - t0, r })
  redraw($)
}

// ---- the worker view, drawn in the one pane while a worker is open ----
async function renderWorker($: EngineInterface, e: RenderArg) {
  const { Box, Text, Button, Input } = $.ui.resolve(e)
  const w = openId ? workers.get(openId) : undefined
  const t = await $.clock.now()
  if (!w) return <Text dimColor>No worker open.</Text>
  const cols = e.props.bodyColumns
  const rows = Math.max(8, e.props.scroll.bodyRows)
  // the saved transcript, as the session holds it
  const t0 = await $.clock.now()
  const msgs = await $.session.messages({ agentId: w.agentId })
  const msgMs = (await $.clock.now()) - t0
  const out: { color?: string; dim?: boolean; text: string }[] = []
  if (Array.isArray(msgs)) {
    for (const m of msgs) {
      if (m.role === 'user' && m.text) out.push({ color: '#d7af00', text: `› ${m.text}` })
      if (m.role === 'assistant' && m.text) out.push({ text: `● ${m.text}` })
      for (const u of m.toolUses ?? []) {
        out.push({ color: '#5f87af', text: `● ${u.tool}(${one(inputSummary(u.input), cols - 12)})` })
        if (u.text !== undefined) out.push({ dim: true, text: `  ⎿ ${one(u.text, cols * 2)}` })
        else out.push({ dim: true, text: '  ⎿ running…' })
      }
    }
  } else out.push({ color: '#d75f5f', text: `messages: ${JSON.stringify(msgs)}` })
  if (w.live) out.push({ text: `● ${w.live}▌` })
  if (w.status !== 'running' && w.answer) out.push({ color: '#008700', text: `Final report: ${w.answer}` })
  const room = Math.max(3, rows - 6)
  const shown = out.slice(-room)
  void log($, 'render.worker', { agentId: w.agentId, sinceEventMs: t - w.lastEventMs, msgMs, msgs: Array.isArray(msgs) ? msgs.length : -1, lines: out.length })
  return (
    <Box flexDirection="column">
      <Box>
        <Text bold>{w.name}</Text>
        <Text color={w.status === 'running' ? '#0087d7' : '#008700'}> {w.status}</Text>
        <Text dimColor> · {dur((w.endedMs ?? t) - w.startedMs)} · {w.tools} tools · last event {dur(t - w.lastEventMs)} ago · {Array.isArray(msgs) ? msgs.length : '?'} msgs read in {msgMs} ms</Text>
      </Box>
      {out.length > shown.length && <Text dimColor>↑ {out.length - shown.length} earlier lines</Text>}
      {shown.map((l, i) => (
        <Text key={`l:${i}`} color={l.color} dimColor={l.dim} wrap="truncate-end">{l.text}</Text>
      ))}
      {w.status === 'running' && (
        <Input key="send" label="to worker › " placeholder="message the worker (inbox)" submitLabel="send" autoFocus onSubmit={async v => {
          if (!v.trim()) return
          const r = await $.session.append({ agentId: w.agentId, message: { type: 'user', content: [{ type: 'text', text: v }] } }).catch(err => ({ error: String(err) }))
          push(w, { t: await $.clock.now(), kind: 'sent', text: `sent: ${v}` })
          await log($, 'append', { agentId: w.agentId, v, r })
          redraw($)
        }} />
      )}
      <Box gap={1}>
        <Button key="back" label="Back (Esc)" onPress={() => back($)} />
        {w.status === 'running' && (
          <Button key="stop" label="Stop worker" onPress={async () => {
            const r = await $.turn.abort({ turnId: w.turnId ?? '' }).then(() => 'ok', err => String(err))
            push(w, { t: await $.clock.now(), kind: 'note', text: `stop: ${r}` })
            await log($, 'abort', { agentId: w.agentId, turnId: w.turnId, r })
            redraw($)
          }} />
        )}
      </Box>
    </Box>
  )
}

function back($: EngineInterface) {
  openId = undefined
  void $.ui.open({ id: LIST, title: 'Workers', closeOnEscape: true })
  redraw($)
}

// ---- the native path: show the strip on one worker (bin/strip.py reads the flag) ----
const FLAG = '/home/ita/giverny/.claude/worktrees/task-186-mod-spike/mods-spike/worker-view/state/strip'
let revealId: string | undefined
let lastView: string | undefined
let enteredView = false
async function reveal($: EngineInterface, id: string) {
  revealId = id
  enteredView = false
  await $.fs.write(FLAG, id)
  await log($, 'reveal', { id })
  redraw($)
}
async function trackView($: EngineInterface, view: string | undefined) {
  if (view === lastView) return
  await log($, 'view', { from: lastView ?? 'main', to: view ?? 'main', revealId })
  lastView = view
  if (revealId && view === revealId) enteredView = true
  // back on main after visiting the revealed worker: hide the strip again
  if (revealId && view === undefined && enteredView) {
    revealId = undefined
    await $.fs.write(FLAG, '')
    await log($, 'hide', {})
  }
  redraw($)
}

async function renderList($: EngineInterface, e: RenderArg) {
  const { Box, Text, Button } = $.ui.resolve(e)
  const t = await $.clock.now()
  const list = [...workers.values()]
  return (
    <Box flexDirection="column">
      {list.length === 0 && <Text dimColor>No workers yet: /wv-spawn</Text>}
      {list.map((w, i) => (
        <Box key={`r:${w.agentId}`}>
          <Button key={`row:${w.agentId}`} plain hotkey={String(i + 1)} label={`${w.status === 'running' ? '●' : '✓'} ${w.name}`} onPress={() => reveal($, w.agentId)} />
          <Button key={`peek:${w.agentId}`} plain hotkey={'abcdefghij'[i]} label="peek" dimColor onPress={() => openWorker($, w.agentId)} />
          <Text color={w.status === 'running' ? '#0087d7' : '#008700'}> {w.status} </Text>
          <Text dimColor>{dur((w.endedMs ?? t) - w.startedMs)} · {w.tools} tools · {one(w.lines.at(-1)?.text ?? '', Math.max(10, e.props.bodyColumns - 40))}</Text>
        </Box>
      ))}
      <Box gap={1}>
        <Button key="tasks" label="Native /tasks" onPress={async () => {
          const r = await $.command.run({ command: 'tasks' }).then(x => ({ ok: x }), err => ({ err: String(err) }))
          note = `/tasks via $.command.run: ${JSON.stringify(r).slice(0, 120)}`
          await log($, 'command.run.tasks', { r })
          redraw($)
        }} />
      </Box>
      {revealId && <Text color="#d7af00">{workers.get(revealId)?.name ?? revealId}: shown in the agent strip under the prompt; Esc, then ↓ ↓ Enter enters it</Text>}
      <Text dimColor>view: {e.props.view.agentId ? (workers.get(e.props.view.agentId)?.name ?? e.props.view.agentId) : 'main'}</Text>
      {note && <Text dimColor wrap="truncate-end">{note}</Text>}
    </Box>
  )
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const out = await next(e)
    bandMode = (await $.env.get('WV_BAND')) === '1'
    await $.agent
      .register({ name: 'worker', description: 'worker-view-spike test worker (spawned by the mod only)', prompt: WORKER_PROMPT, tools: ['Bash'], model: 'haiku', omitClaudeMd: true })
      .then(r => log($, 'agent.register', { r }), err => log($, 'agent.register.error', { err: String(err) }))
    await $.command.register({ name: 'wv-spawn', description: 'Spawn 2 test workers from the mod', immediate: true })
    await $.command.register({ name: 'wv-hide', description: 'Hide the native subagent panel: point this project\'s settings.local.json subagentStatusLine at bin/strip.py', immediate: true })
    await $.command.register({ name: 'wv', description: 'Open the workers pane', immediate: true })
    await $.command.register({ name: 'wv-open', description: 'Open worker N (1-based) in the worker pane', argumentHint: '<n>', immediate: true })
    // a 1 s tick keeps the elapsed clocks moving
    $.clock.every(1000, () => redraw($))
    return out
  })

  on('agent.offer', { agent: WORKER }, () => ({ isOffered: false }))

  on('command.run', { command: 'wv-spawn' }, async ($, e) => {
    const n = Number(e.args || 2)
    const rs = await Promise.all(Array.from({ length: n }, (_, i) => spawnOne($, `w${i + 1}`)))
    if (!bandMode) await $.ui.open({ id: LIST, title: 'Workers' })
    redraw($)
    return { text: `spawned ${rs.map(r => r.agentId ?? r.deny).join(', ')}` }
  })

  on('command.run', { command: 'wv-hide' }, async $ => {
    const f = `${await $.session.cwd()}/.claude/settings.local.json`
    let cur: Record<string, unknown> = {}
    if (await $.fs.exists(f)) cur = JSON.parse(await $.fs.read(f))
    cur.subagentStatusLine = { type: 'command', command: `${HERE}/bin/strip.py` }
    await $.fs.write(f, JSON.stringify(cur, null, 2) + '\n')
    await $.fs.write(FLAG, '')
    await log($, 'wv-hide', { f })
    return { text: `wrote subagentStatusLine into ${f}` }
  })

  on('command.run', { command: 'wv' }, async $ => {
    const r = await $.ui.open({ id: LIST, title: 'Workers', focus: true })
    return { text: r.isPlaced ? 'Workers pane opened.' : `not placed: ${r.reason}` }
  })

  on('command.run', { command: 'wv-open' }, async ($, e) => {
    const w = [...workers.values()][Number(e.args || 1) - 1]
    if (!w) return { text: 'no such worker' }
    await openWorker($, w.agentId)
    return { text: `opened ${w.name}` }
  })

  // ---- the worker's live activity ----
  on('tool.call', async ($, e, next) => {
    const w = e.agentId ? workers.get(e.agentId) : undefined
    if (!w) return next(e)
    const t0 = await $.clock.now()
    const what = inputSummary(e as unknown as Record<string, unknown>)
    w.tools++
    push(w, { t: t0, kind: 'tool', text: `${e.tool}(${one(what, 200)})` })
    await log($, 'tool.start', { agentId: w.agentId, tool: e.tool, what: one(what, 80) })
    redraw($)
    const r = await next(e)
    const t1 = await $.clock.now()
    const out = 'text' in r && typeof r.text === 'string' ? r.text : 'deny' in r && r.deny ? `denied: ${r.deny}` : JSON.stringify((r as { result?: unknown }).result ?? '')
    push(w, { t: t1, kind: 'tool-end', text: `${'isError' in r && r.isError ? '✗' : '✓'} ${e.tool} ${dur(t1 - t0)}: ${one(out ?? '', 300)}` })
    await log($, 'tool.end', { agentId: w.agentId, tool: e.tool, ms: t1 - t0 })
    redraw($)
    return r
  })

  on('turn.step', async function* ($, e, next) {
    const w = e.agentId ? workers.get(e.agentId) : undefined
    if (!w) return yield* next(e)
    w.turnId = e.turnId
    w.live = ''
    let chunks = 0
    const t0 = await $.clock.now()
    push(w, { t: t0, kind: 'step', text: `step ${e.index} → ${e.model}` })
    const it = next(e)
    for (;;) {
      const n = await it.next()
      if (n.done) {
        const res = n.value
        if (w.live.trim()) push(w, { t: await $.clock.now(), kind: 'text', text: w.live.trim() })
        w.live = ''
        await log($, 'step.end', { agentId: w.agentId, index: e.index, chunks, ms: (await $.clock.now()) - t0 })
        redraw($)
        return res
      }
      const c = n.value
      if (c.kind === 'text') {
        chunks++
        w.live += c.text
        w.lastEventMs = await $.clock.now()
        redraw($)
      }
      yield c
    }
  })

  on('turn.complete', async ($, e, next) => {
    const out = await next(e)
    const w = e.agentId ? workers.get(e.agentId) : undefined
    if (w) {
      const t = await $.clock.now()
      w.status = e.reason === 'answer' ? 'done' : e.reason === 'aborted' ? 'aborted' : 'error'
      w.endedMs = t
      w.answer = e.answer
      push(w, { t, kind: 'done', text: `${e.reason}: ${e.answer}` })
      await log($, 'turn.complete', { agentId: w.agentId, reason: e.reason, answer: e.answer.slice(0, 200) })
      redraw($)
    }
    return out
  })

  on('ui.close', { id: LIST }, async ($, e, next) => {
    await log($, 'ui.close', { origin: e.origin, openId })
    if (openId && e.origin.kind === 'person') {
      back($)
      return {} as never
    }
    return next(e)
  })

  // ---- the list pane ----
  on('ui.render', { component: 'Pane', requestId: LIST }, async ($, e) => {
    await trackView($, e.props.view.agentId)
    if (openId) return renderWorker($, e)
    return renderList($, e)
  })

  // the band variant (WV_BAND=1): the list above the prompt, full width, where the agent strip sits
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (!bandMode || workers.size === 0) return next(e)
    await trackView($, e.props.view.agentId)
    return renderList($, e)
  })

}
