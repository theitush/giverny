import type { EngineInterface, Register } from 'claude-code'

import { barMoved, dragged, drawBand, loadSize, ownWindow, scrolled, sizeCommand, switchesOf } from '../src/band'
import { answerMerge, hintWithoutAgents, insideGivernyTab, letManagerThrough } from '../src/giverny'
import { clearDone, startPolling } from '../src/poll'
import { readPaged, toggleHidden } from '../src/state'
import { drawPage, drawPageRow, goBack, hiddenRow, noteScroll, PAGE_ROW, pageClosed, pageLine, PANE, startFollowing, TRANSCRIPT_ROWS } from '../src/worker'

// Wiring only: what each event runs lives in src/. Inside a Giverny tab every hook passes.

const AFTER_RELOAD_MS = 1000
const GRIP = 'grip'
const BAR = 'bar'

let begun = false
let beginSoon = false

async function begin($: EngineInterface, wantUse: boolean): Promise<void> {
  if (begun) return
  begun = true
  try {
    await $.command.register({
      name: 'manager',
      description: 'Show or hide the manager band: Running, Next up and Done from this session; `/manager back` leaves a worker for the manager; `/manager clear-done` clears its Done rows; `/manager size <rows>` or `auto` sets its height',
    })
    await loadSize($)
    await startPolling($, wantUse)
    startFollowing($)
  } catch (err) {
    // Before the session binds: `session.start`, or the next drawing, begins it.
    begun = false
    throw err
  }
}

export const register: Register = (on, options) => {
  const switches = switchesOf(options)

  on('session.start', async ($, e, next) => {
    if (await insideGivernyTab($)) return next(e)
    await begin($, switches.usage)
    return next(e)
  })

  on('command.run', { command: 'manager' }, async ($, e) => {
    if (e.args.trim() === 'clear-done') return { text: await clearDone($) }
    if (e.args.trim() === 'back') {
      await goBack($)
      return { text: 'Back to the manager.' }
    }
    const size = /^size(?:\s+(.*))?$/.exec(e.args.trim())
    if (size) return { text: await sizeCommand($, (size[1] ?? '').trim()) }
    const isHidden = await toggleHidden($)
    return { text: isHidden ? 'Manager band hidden.' : 'Manager band shown.' }
  })

  on('settings.read', async ($, e, next) => {
    const settings = await next(e)
    return e.source ? settings : answerMerge(settings, await insideGivernyTab($))
  })

  on('tool.check', { tool: 'Bash' }, async ($, e, next) => {
    const beneath = await next(e)
    return (await insideGivernyTab($)) ? beneath : letManagerThrough($, e.input, beneath)
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, ($, e) => drawPage($, e))

  // A page takes the transcript's place in fullscreen: a row of its own, and every other row drawn as nothing.
  on('ui.render', { component: 'Placed' }, ($, e, next) => (e.props.id === PAGE_ROW ? drawPageRow($, e) : next(e)))
  for (const component of TRANSCRIPT_ROWS) {
    on('ui.render', { component }, async ($, e, next) => ((await readPaged($)) ? hiddenRow($, e) : next(e)))
  }

  on('prompt.submit', async ($, e, next) => {
    const line = await pageLine($, e)
    if (line === undefined) return next(e)
    return 'drop' in line ? line : next({ ...e, text: line.text })
  })

  on('ui.close', async ($, e, next) => {
    if (e.id === PANE) await pageClosed($)
    return next(e)
  })

  on('ui.scroll', { requestId: PANE }, ($, e, next) => {
    noteScroll(e)
    return next(e)
  })

  // A change in the config menu reloads this module, which drops the timers, and no `session.start`
  // comes after it: the first drawing after a load starts them again. Not from the drawing itself,
  // which may change no state: from a timer, once it has settled.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (!begun && !beginSoon && !(await insideGivernyTab($))) {
      beginSoon = true
      $.clock.after(AFTER_RELOAD_MS, () => {
        beginSoon = false
        return begin($, switches.usage).catch(() => {})
      })
    }
    return (await drawBand($, e, switches)) ?? next(e)
  })

  on('ui.message', { element: GRIP }, async ($, e) => {
    await dragged($, e.data)
    return {}
  })

  on('ui.message', { element: BAR }, async ($, e) => {
    await barMoved($, e.data)
    return {}
  })

  on('ui.scroll', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (!ownWindow() || (await insideGivernyTab($))) return next(e)
    await scrolled($, e.by)
    return {}
  })

  on('ui.render', { component: 'PromptHint' }, async ($, e, next) => {
    const hint = hintWithoutAgents(e.props.hint)
    return hint === e.props.hint || (await insideGivernyTab($)) ? next(e) : next({ ...e, props: { ...e.props, hint } })
  })
}
