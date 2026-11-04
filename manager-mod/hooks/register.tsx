import type { Register } from 'claude-code'

import { drawBand } from '../src/band'
import { startPolling } from '../src/poll'
import { toggleHidden } from '../src/state'

// Wiring only: what each event runs lives in src/.
export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'manager',
      description: 'Show or hide the manager band: Running, Next up and Done from this session',
    })
    await startPolling($)
    return next(e)
  })

  on('command.run', { command: 'manager' }, async $ => {
    const isHidden = await toggleHidden($)
    return { text: isHidden ? 'Manager band hidden.' : 'Manager band shown.' }
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => (await drawBand($, e)) ?? next(e))
}
