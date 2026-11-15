import type { EngineInterface, Register } from 'claude-code'

// Written by Giverny (crates/claude/src/view_ask.rs, giverny#525): how a click on a management-panel
// row opens that worker's view in this tab's Claude Code. `$.ui.view` moves the view only while it
// answers the person's own input (a press, their prompt or command): from a timer, or a `prompt.edit`,
// it is refused as "not person-initiated". So Giverny writes an ask into this tab's file, this module
// draws a Button for it in the hint line under the prompt, and Giverny clicks that Button: the press is
// the person's input, nothing is typed, and the prompt, its draft and its cursor are left as they were.
// Giverny holds the tab's picture while it does, so the Button is never seen.

const DIR = __GIVERNY_VIEW_DIR__
/** How often the tab's ask file is read. */
const POLL_MS = 150
/** An ask older than this is not drawn: one left behind by a Giverny that went away. */
const ASK_TTL_MS = 5000

type Ask = { nonce: string; agent?: string | null; at: number }

let tab: string | undefined
let shown: Ask | undefined
let answered = ''

/** The tab's ask, if one is waiting: drawn, or taken down once withdrawn, answered or stale. */
async function look($: EngineInterface): Promise<void> {
  let ask: Ask | undefined
  try {
    const got = JSON.parse(await $.fs.read(`${DIR}/${tab}.json`)) as Ask
    const fresh = typeof got.at === 'number' && (await $.clock.now()) - got.at < ASK_TTL_MS
    if (typeof got.nonce === 'string' && got.nonce !== answered && fresh) ask = got
  } catch {
    ask = undefined
  }
  if (ask?.nonce !== shown?.nonce) {
    shown = ask
    $.ui.invalidate('ui.render')
  }
}

/** Giverny's click: the view moves to the worker (or back to the main conversation), and it is told how that went. */
async function press($: EngineInterface, ask: Ask): Promise<void> {
  answered = ask.nonce
  shown = undefined
  $.ui.invalidate('ui.render')
  const r = await $.ui.view(ask.agent ? { agentId: ask.agent } : {})
  await $.fs.write(`${DIR}/${tab}.reply.json`, JSON.stringify({ nonce: ask.nonce, deny: r.deny ?? null }))
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    if (e.isInteractive && tab === undefined) {
      const id = await $.env.get('GIVERNY_TAB_ID')
      if (id !== undefined && /^[A-Za-z0-9_-]{1,64}$/.test(id)) {
        tab = id
        $.clock.every(POLL_MS, () => void look($))
      }
    }
    return next(e)
  })

  on('ui.render', { component: 'PromptHint' }, async ($, e, next) => {
    const ask = shown
    if (ask === undefined) return next(e)
    const { Box, Button } = $.ui.resolve(e)
    return (
      <Box>
        <Button key={`giverny-open-${ask.nonce}`} plain label={`giverny-open:${ask.nonce}`} onPress={() => press($, ask)} />
      </Box>
    )
  })
}
