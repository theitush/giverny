import type { EngineInterface } from 'claude-code'

import type { AgentRow, FeedRow } from '../types'

import { feedDir, newWatch, pollFeed } from './feed'
import { givernyHookOn } from './giverny'
import { newLive, readAgents, readAround } from './live'
import { buildModel, clearable, dueChores } from './model'
import { readCleared, readFolded, readHidden, writeCleared, writeView } from './state'

const POLL_MS = 1000
const CHORES_MS = 10_000
const EMPTY = JSON.stringify(buildModel(undefined, [], 0))

/** Polls now, for a change the person just made. */
let pollNow: () => Promise<void> = async () => {}
let finishedAgents: () => string[] = () => []
let polling = false

/**
 * Keeps `view` current. A poll costs a few `stat`s and `$.agent.list()`. ELAPSED is a stopwatch,
 * as the panel's, so a Running row redraws the band each second; with none running a `view` that
 * does not differ is not written, so the band is not redrawn.
 */
export async function startPolling($: EngineInterface, wantUse = true): Promise<void> {
  // A reload drops the timers and leaves no `session.start` to set them again (register.tsx), and
  // the module's own state goes with them; so this holds for one load.
  if (polling) return
  polling = true
  const watch = newWatch()
  const live = newLive()
  const dev = Boolean(await $.env.get('MANAGER_MOD_DEV'))
  let shown = ''
  let busy = false
  let choresAt = 0
  let choresWritten = ''
  let agents: AgentRow[] = []
  let held: FeedRow[] = []
  const poll = async () => {
    if (busy) return
    busy = true
    try {
      const now = await $.clock.now()
      const session = await $.session.id()
      const feed = await pollFeed($, watch, now)
      const sessions = [...new Set([session, ...(feed?.session ? [feed.session] : []), ...(feed?.aliases ?? [])])]
      held = feed?.rows ?? []
      agents = await readAgents($, live, sessions, now)
      if (dev) agents = [...agents, ...(feed?.dev_agents ?? [])]
      // Nothing to draw: nothing to look up either.
      if ((feed?.rows.length ?? 0) === 0 && agents.length === 0) {
        if (shown !== EMPTY) {
          shown = EMPTY
          await writeView($, buildModel(undefined, [], now))
        }
        return
      }
      // Figures nothing draws cost nothing: the column off, or the band hidden.
      const around = await readAround($, live, session, feed, agents, now, wantUse && !(await readHidden($)))
      const shells = { ...around.shells }
      for (const a of dev ? (feed?.dev_agents ?? []) : []) {
        if (a.shells) shells[a.id] = a.shells
      }
      const cleared = await readCleared($)
      const full = { ...around, shells, folded: await readFolded($), clearedMs: cleared.atMs, dismissed: cleared.agents }
      if (dev && feed?.dev_use) full.use = { tasks: Object.fromEntries(Object.entries(feed.dev_use).map(([key, use]) => [key, { use }])), workers: {}, ramMb: 32768 }
      const model = buildModel(feed, agents, now, full)
      const drawn = JSON.stringify(model)
      if (drawn !== shown) {
        shown = drawn
        await writeView($, model)
      }
      if (now - choresAt >= CHORES_MS && around.manager && feed?.writer === 'giverny/manage') {
        choresAt = now
        const due = dueChores(feed, agents, now, full)
        // With no Giverny hook to read it, the file would only be litter.
        const text = due.chores.length === 0 || !(await givernyHookOn($)) ? '' : JSON.stringify(due.finishing.length === 0 ? { chores: due.chores } : due)
        if (text !== choresWritten) choresWritten = await publishChores($, session, text)
      }
    } finally {
      busy = false
    }
  }
  pollNow = poll
  finishedAgents = () => clearable(held, agents)
  await poll()
  $.clock.every(POLL_MS, poll)
}

/**
 * Keeps `<feed dir>/chores/<session>.json` for the giverny-hook to tell the manager, as
 * Giverny's app does for its own tabs; in one of those the app's is the one kept. It tells,
 * and nothing else: the manager decides. Returns what the file now holds.
 */
async function publishChores($: EngineInterface, session: string, text: string): Promise<string> {
  if (await $.env.get('GIVERNY_TAB_ID')) return text
  const path = `${await feedDir($)}/chores/${session}.json`
  try {
    if (text === '') {
      await $.process.run(['rm', '-f', path])
    } else {
      await $.fs.write(`${path}.tmp`, text)
      await $.process.run(['mv', '-f', `${path}.tmp`, path])
    }
  } catch {
    // The next change tries again.
    return ''
  }
  return text
}

/** `/manager clear-done`: `giverny-manage clear-done`, and the rows it cannot reach go too. */
export async function clearDone($: EngineInterface): Promise<string> {
  const session = await $.session.id()
  const config = (await $.env.get('XDG_CONFIG_HOME')) || `${await $.env.get('HOME')}/.config`
  const bin = `${config}/giverny/claude-plugin/plugins/giverny/bin/giverny-manage`
  const command = (await $.fs.exists(bin)) ? bin : 'giverny-manage'
  let said: string
  try {
    const r = await $.process.run([command, 'clear-done'], { env: { CLAUDE_CODE_SESSION_ID: session } })
    said = `${r.stdout}${r.stderr}`.trim()
  } catch (e) {
    // No Giverny here: there is no feed of its to clear, and the band hides the rest below.
    said = /not found|ENOENT/.test(String(e)) ? '' : `giverny-manage did not run: ${e instanceof Error ? e.message : String(e)}`
  }
  // Another writer's feed keeps its Done rows, and Claude Code still lists finished workers:
  // the band hides what landed by now, and they do not come back.
  await writeCleared($, { atMs: await $.clock.now(), agents: finishedAgents() })
  await pollNow()
  return said || 'Done rows cleared.'
}

/** After a fold: redraw now, not on the next poll. */
export function redrawSoon(): Promise<void> {
  return pollNow()
}
