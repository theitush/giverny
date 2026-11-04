import type { EngineInterface } from 'claude-code'

import { newWatch, pollFeed } from './feed'
import { buildModel } from './model'
import { writeView } from './state'
import { minuteOf } from './time'

const POLL_MS = 2000

/**
 * Keeps `view` current. A poll costs a stat and `$.agent.list()`; the clocks count from the
 * start of the minute, so nothing differs between minutes unless the feed or a worker does,
 * and a `view` that does not differ is not written, so the band is not redrawn.
 */
export async function startPolling($: EngineInterface): Promise<void> {
  const watch = newWatch()
  let shown = ''
  let busy = false
  const poll = async () => {
    if (busy) return
    busy = true
    try {
      const now = await $.clock.now()
      const feed = await pollFeed($, watch, now)
      const agents = await $.agent.list()
      const model = buildModel(feed, agents, minuteOf(now))
      const drawn = JSON.stringify(model)
      if (drawn !== shown) {
        shown = drawn
        await writeView($, model)
      }
    } finally {
      busy = false
    }
  }
  await poll()
  $.clock.every(POLL_MS, poll)
}
