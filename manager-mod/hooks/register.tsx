import type { Register } from 'claude-code'

import { drawBand } from '../src/band'
import { answerMerge, hintWithoutAgents, insideGivernyTab, letManagerThrough } from '../src/giverny'
import { clearDone, startPolling } from '../src/poll'
import { toggleHidden } from '../src/state'
import { drawPage, goBack, noteScroll, PANE, pageClosed, startFollowing } from '../src/worker'

// Wiring only: what each event runs lives in src/. Inside a Giverny tab every hook passes.
export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    if (await insideGivernyTab($)) return next(e)
    await $.command.register({
      name: 'manager',
      description: 'Show or hide the manager band: Running, Next up and Done from this session; `/manager back` leaves a worker for the manager; `/manager clear-done` clears its Done rows',
    })
    await startPolling($)
    startFollowing($)
    return next(e)
  })

  on('command.run', { command: 'manager' }, async ($, e) => {
    if (e.args.trim() === 'clear-done') return { text: await clearDone($) }
    if (e.args.trim() === 'back') {
      await goBack($)
      return { text: 'Back to the manager.' }
    }
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

  on('ui.close', async ($, e, next) => {
    if (e.id === PANE) await pageClosed($)
    return next(e)
  })

  on('ui.scroll', { requestId: PANE }, ($, e, next) => {
    noteScroll(e)
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => (await drawBand($, e)) ?? next(e))

  on('ui.render', { component: 'PromptHint' }, async ($, e, next) => {
    const hint = hintWithoutAgents(e.props.hint)
    return hint === e.props.hint || (await insideGivernyTab($)) ? next(e) : next({ ...e, props: { ...e.props, hint } })
  })
}
