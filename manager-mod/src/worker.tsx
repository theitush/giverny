import type { EngineInterface, PromptSubmitInput, RenderInput, RenderViewport, SiteView, UiScrollInput } from 'claude-code'

import type { DisplayRow, Model, Page, PageEntry } from '../types'

import { cells } from './cells'
import { askLine, entriesOf, entriesOfFile, inRoom, noBrief, pageRows, pendingAfter, startLine } from './page'
import { changePage, readPage, readPaged, readView, say, writePage, writePaged } from './state'

// What a press on a row opens. A worker Claude Code still lists opens in Claude Code's own view
// of it, `$.ui.view`, whose prompt is that worker's; every other row opens a page, as Giverny's
// worker page does (docs/management-panel.md, What a click on a row opens). In fullscreen the page
// takes the transcript's place, so it reads as that view does: a placed row draws it and every
// other row of the transcript draws nothing while it is open. On the main screen, where what is
// drawn cannot be taken back, it is a pane.

export const PANE = 'worker'
export const PAGE_ROW = 'manager-page'
/** What a transcript is made of, all a hook can draw: the logo is not among them, and stays above a page as above a view. */
export const TRANSCRIPT_ROWS = ['UserMessage', 'AssistantMessage', 'ToolUse', 'ToolResult', 'ToolGroup', 'ToolProgress', 'CommandOutput', 'Spinner', 'TurnDuration', 'InfoNotice'] as const
const FOLLOW_MS = 1000
const LAID_OUT_MS = [150, 450]
// Claude Code's own prompt band, as it draws one in a worker's view.
const PROMPT_BG = 'ansi256(237)'

/** While the person has scrolled up, the page stays where they put it. */
let pinned = false
let reading = false
let fileMs: number | undefined

/** A press on a row: its worker's view, or its page; on what is open, back to the manager. */
export async function openRow($: EngineInterface, r: DisplayRow, view: SiteView | undefined, band: string, screen: RenderViewport | undefined): Promise<void> {
  const page = await readPage($)
  const onIt = page?.rowId === r.id || (r.workerId !== undefined && view?.agentId === r.workerId)
  if (onIt) return goBack($, view)
  if (r.workerId !== undefined) {
    const shown = await $.ui.view({ agentId: r.workerId })
    if (shown.deny === undefined) {
      if (page) await closePage($)
      await say($, '')
      return
    }
  }
  // A page beside a worker's view would leave the prompt talking to that worker.
  if (view?.agentId !== undefined) await $.ui.view({})
  await openPage($, r, band, screen)
}

/**
 * The band's back button (a bare 0 at the empty prompt presses it), the page's, `/manager back`.
 * Esc is not this in a worker's view: Claude Code's Esc at that prompt stops the worker, and a
 * plugin cannot take that key, so the band says so (backWords).
 */
export async function goBack($: EngineInterface, view?: SiteView): Promise<void> {
  if (await readPage($)) await closePage($)
  if (view === undefined || view.agentId !== undefined) {
    const r = await $.ui.view({})
    await say($, r.deny === undefined ? '' : `back refused: ${r.deny}`)
    return
  }
  await say($, '')
}

async function openPage($: EngineInterface, r: DisplayRow, band: string, screen: RenderViewport | undefined): Promise<void> {
  const ask = r.own === 'next' ? ('start' as const) : r.workerId !== undefined ? ('worker' as const) : undefined
  const placed = screen?.isFullscreen === true ? await $.ui.place({ id: PAGE_ROW }) : undefined
  const page: Page = { place: placed?.isPlaced ? 'row' : 'pane', band, rowId: r.id, row: r, entries: await firstEntries($, r), ask, pending: [], prompts: 0, status: '', draft: '', box: 0 }
  page.prompts = page.entries.filter(e => e.kind === 'prompt').length
  await writePage($, page)
  pinned = false
  fileMs = undefined
  await say($, '')
  if (placed?.isPlaced) {
    await writePaged($, true)
    // A brief too long for the screen opens at its top. That takes the transcript off its end, which
    // it finds again at the next prompt or a scroll to the bottom. All else is at its end already,
    // and a transcript follows its end the engine's own way.
    if (r.own === 'next' && pageRows(page.entries, screen?.columns ?? 0) >= (screen?.rows ?? 0)) await reveal($, placed.requestId)
    return
  }
  const opened = await $.ui.open({ id: PANE, title: r.key || r.title, focus: true, closeOnEscape: true })
  if (!opened.isPlaced) {
    await say($, `no room for the page: ${opened.reason}`)
    return
  }
  // A press leaves the keys with the band, and a site that holds none is refused them, so the
  // line for the worker is typed in the band, where they already are.
  if (ask) await toBox($, band, 'say-0')
  if (r.own !== 'next') await toEnd($)
}

/**
 * The page's first line at the top of the transcript's window, once the row has been laid out at
 * its height, which takes longer the longer the transcript; still within the press, as the
 * transcript moves only for the person's own input. A nicety: else it stays where it is.
 */
async function reveal($: EngineInterface, requestId: string): Promise<void> {
  try {
    for (const ms of LAID_OUT_MS) {
      await $.clock.sleep(ms)
      await $.ui.scroll({ to: { requestId }, block: 'start' })
    }
  } catch {
    // Left where it is.
  }
}

async function firstEntries($: EngineInterface, r: DisplayRow): Promise<PageEntry[]> {
  if (r.own === 'next' && r.brief !== undefined) {
    try {
      return [{ kind: 'prompt', text: await $.fs.read(r.brief) }]
    } catch (e) {
      return [{ kind: 'tool', text: `Its brief ${r.brief} cannot be read: ${e instanceof Error ? e.message : String(e)}`, isError: true }]
    }
  }
  if (r.own === 'next') return [{ kind: 'prompt', text: noBrief(r.key, r.title, r.note) }]
  const read = await readEntries($, r)
  if (read && read.length > 0) return read
  const lines = [r.key && `Task: ${r.key}`, r.title !== r.key ? r.title : '', r.note ?? ''].filter(Boolean).join('\n\n')
  const out: PageEntry[] = [{ kind: 'prompt', text: lines || 'This row has no worker and no transcript.' }]
  for (const w of r.why) out.push({ kind: 'tool', text: w })
  // Giverny runs a row's `open` in a new tab; a plugin has no tabs to open.
  if (r.open !== undefined) out.push({ kind: 'tool', text: `To open it: ${r.open}` })
  return out
}

/** A worker's entries: what the session holds of it, else its transcript file. */
async function readEntries($: EngineInterface, r: DisplayRow): Promise<PageEntry[] | undefined> {
  if (r.workerId !== undefined) {
    const found = await $.session.messages({ agentId: r.workerId })
    if (!('deny' in found)) return entriesOf(found)
  }
  if (r.transcript === undefined) return undefined
  try {
    const { mtimeMs } = await $.fs.stat(r.transcript)
    if (mtimeMs === fileMs) return undefined
    const text = await $.fs.read(r.transcript)
    fileMs = mtimeMs
    return entriesOfFile(text)
  } catch {
    return undefined
  }
}

/** Scrolling is a nicety: a page that cannot move is still the page. */
async function toEnd($: EngineInterface): Promise<void> {
  try {
    await $.ui.scroll({ in: PANE, to: 'end' })
  } catch {
    // Left where it is.
  }
}

/** The keys to the box; refused (the person moved on), the prompt keeps them. */
async function toBox($: EngineInterface, band: string, key: string): Promise<void> {
  try {
    await $.ui.focus({ requestId: band, key })
  } catch {
    // They stay where they are.
  }
}

/** Keeps an open page on its transcript. Costs nothing while none is open. */
export function startFollowing($: EngineInterface): void {
  $.clock.every(FOLLOW_MS, () => followPage($))
}

async function followPage($: EngineInterface): Promise<void> {
  if (reading) return
  const page = await readPage($)
  if (!page || page.row.own === 'next') return
  reading = true
  try {
    const entries = await readEntries($, page.row)
    if (!entries || JSON.stringify(entries) === JSON.stringify(page.entries)) return
    await changePage($, now => {
      const pending = pendingAfter(now.pending, now.prompts, entries)
      const prompts = pending.length < now.pending.length ? entries.filter(e => e.kind === 'prompt').length : now.prompts
      return { ...now, entries, pending, prompts }
    })
    if (!pinned && page.place === 'pane') await toEnd($)
  } catch {
    // The next look tries again: a worker's transcript comes and goes as it is written.
  } finally {
    reading = false
  }
}

/** The person scrolled the page: it follows again once they are back on its end. */
export function noteScroll(e: UiScrollInput): void {
  if (e.origin.kind !== 'person') return
  pinned = e.offset + e.by + e.bodyRows < e.contentRows
}

/** The person closed it: Esc, its mark, ctrl+x x. A plugin's own close does not raise this. */
export async function pageClosed($: EngineInterface): Promise<void> {
  await writePage($, undefined)
}

async function closePage($: EngineInterface): Promise<void> {
  const page = await readPage($)
  await writePage($, undefined)
  await writePaged($, false)
  if (page?.place === 'pane') await $.ui.close({ id: PANE })
}

/** A line for the row's worker, or for a Next up row, the manager: the person's line only. What it came to, and whether it went. */
async function deliver($: EngineInterface, page: Page, text: string): Promise<{ sent: boolean; status: string }> {
  if (page.ask === 'worker' && page.row.workerId !== undefined) {
    const r = await $.session.send({ to: { agentId: page.row.workerId }, text })
    return { sent: r.isDelivered, status: r.isDelivered ? 'Sent to this worker.' : `Not sent: ${r.reason}` }
  }
  try {
    await $.prompt.submit({ text: startLine(page.row.key, text), asUser: true })
    return { sent: true, status: 'Sent to the manager, to start this task with it.' }
  } catch (e) {
    return { sent: false, status: `Not sent: ${e instanceof Error ? e.message : String(e)}` }
  }
}

/** The box's Enter, in a pane's page. */
async function send($: EngineInterface, page: Page, text: string): Promise<void> {
  if (text.trim() === '') return
  const { sent, status } = await deliver($, page, text)
  const box = page.box + 1
  await changePage($, now => ({
    ...now,
    status,
    box,
    draft: sent ? '' : text,
    pending: sent && page.ask === 'worker' ? [...now.pending, text] : now.pending,
  }))
  await toBox($, page.band, `say-${box}`)
  if (!pinned) await toEnd($)
}

/**
 * The prompt's Enter on a page in the transcript's place: the person's own line goes where the
 * page says, as a worker's view sends it to that worker. Undefined: not this page's to take.
 */
export async function pageLine($: EngineInterface, e: PromptSubmitInput): Promise<{ text: string } | { drop: string } | undefined> {
  if (e.command !== undefined || e.isMeta === true || e.origin?.kind !== 'composer') return undefined
  const page = await readPage($)
  if (page?.place !== 'row') return undefined
  if (page.ask === undefined) return { drop: 'This page is read-only: press 0 to go back to the manager first.' }
  if (page.ask === 'start') {
    await say($, 'Sent to the manager, to start this task with it.')
    return { text: startLine(page.row.key, e.text) }
  }
  const { sent, status } = await deliver($, page, e.text)
  await say($, status)
  if (sent) {
    await changePage($, now => ({ ...now, pending: [...now.pending, e.text] }))
    await $.prompt.fill({ text: '' })
  }
  // A drop keeps the draft, for a line that did not go; Claude Code says so in the transcript.
  return { drop: status }
}

/** A transcript's row while a page is in its place. */
export function hiddenRow($: EngineInterface, e: RenderInput) {
  const { Box } = $.ui.resolve(e)
  return <Box display="none" />
}

/** The placed row, which stands in the transcript for good and is only ever the open page. */
export async function drawPageRow($: EngineInterface, e: RenderInput<'Placed'>) {
  return (await readPaged($)) ? drawPage($, e) : hiddenRow($, e)
}

/** The page's tree. */
export async function drawPage($: EngineInterface, e: RenderInput<'Pane'> | RenderInput<'Placed'>) {
  const { Box, Text, Button, Markdown } = $.ui.resolve(e)
  const page = await readPage($)
  if (!page) return <Text dimColor>Nothing open.</Text>
  const model = await readView($)
  const r = model.rows.find(one => one.id === page.rowId) ?? page.row
  const { shown, left } = inRoom(page.entries)
  const entry = (one: PageEntry) => {
    if (one.kind === 'prompt') {
      return (
        <Box backgroundColor={PROMPT_BG} paddingRight={1}>
          <Text wrap="wrap">{`❯ ${one.text}`}</Text>
        </Box>
      )
    }
    if (one.kind === 'tool') return <Text color={one.isError ? 'red' : undefined} dimColor={!one.isError} wrap="truncate">{`  ${one.text}`}</Text>
    return (
      <Box flexDirection="row">
        <Text>{'● '}</Text>
        <Box flexDirection="column" flexGrow={1} flexShrink={1}>
          <Markdown text={one.text} />
        </Box>
      </Box>
    )
  }
  return (
    <Box flexDirection="column" gap={1}>
      <Box flexDirection="column">
        <Box flexDirection="row" gap={2}>
          <Text bold>{r.key || r.title}</Text>
          {page.place === 'pane' && (
            <Button key="back" hotkey="0" plain onPress={() => goBack($)}>
              back to the manager
            </Button>
          )}
        </Box>
        {r.key !== '' && r.title !== r.key && <Text dimColor>{r.title}</Text>}
        {r.facts.length > 0 && <Text dimColor>{r.facts.join(' · ')}</Text>}
        {r.review !== undefined && <Text color="yellow">{`Review: ${r.review}`}</Text>}
      </Box>
      {left > 0 && <Text dimColor>{`${left} earlier entries are not drawn: a pane draws so much text.`}</Text>}
      {shown.map(entry)}
      {page.pending.map(line => (
        <Text dimColor wrap="wrap">{`❯ ${line}`}</Text>
      ))}
      {page.ask === undefined && <Text dimColor>read-only</Text>}
    </Box>
  )
}

/** The page's box, drawn in the band: what is typed in it goes to the row's worker or manager. */
export async function drawBox($: EngineInterface, e: RenderInput<'AbovePrompt'>) {
  const page = await readPage($)
  if (!page?.ask || page.place !== 'pane') return undefined
  const { Box, Text, Input } = $.ui.resolve(e)
  const ask = page.ask
  const bad = page.status.startsWith('Not sent')
  return (
    <Box flexDirection="column">
      <Input
        key={`say-${page.box}`}
        label={page.row.key || 'worker'}
        placeholder={askLine(ask)}
        value={page.draft}
        submitLabel="send"
        onSubmit={text => send($, page, text)}
      />
      {page.status !== '' && (
        <Text color={bad ? 'red' : undefined} dimColor={!bad}>
          {page.status}
        </Text>
      )}
    </Box>
  )
}

/**
 * What the row of the back button says beside it, in `room` cells: where a line typed on a page
 * goes, the other ways back, and what Esc stops: a running worker in its view, the manager's turn
 * on a page. A plugin cannot change that, as no key reaches it.
 */
export function backWords(model: Model, view: SiteView | undefined, page: Page | undefined, room: number, said = '', working = false): string {
  const r = view?.agentId !== undefined ? model.rows.find(one => one.workerId === view.agentId) : undefined
  const esc = view?.agentId !== undefined ? (r?.own === 'running' ? 'this worker' : undefined) : page?.place === 'row' && working ? "the manager's turn" : undefined
  const line = page?.place !== 'row' ? '' : page.ask === 'start' ? 'a line at the prompt starts this task' : page.ask === 'worker' ? 'a line at the prompt goes to this worker' : 'read-only'
  const ways = ['or press 0 at the empty prompt, or click its row again', 'or press 0, or click its row again', 'or press 0']
  const tries = ways.flatMap(way => [
    [line, way, esc && `Esc at the prompt stops ${esc}`],
    [line, way, esc && `Esc stops ${esc}`],
  ]).concat([[way0(line), 'or press 0', esc && `Esc stops ${esc}`], ['or press 0', esc && 'Esc stops it'], [esc ? 'Esc stops it' : 'or press 0']])
  const fit = tries.map(t => t.filter(Boolean).join('  ·  ')).find(t => cells(t) <= room - (said ? cells(said) + 5 : 0)) ?? ''
  return said ? [said, fit].filter(Boolean).join('  ·  ') : fit
}

const way0 = (line: string) => (line === 'read-only' ? line : '')
