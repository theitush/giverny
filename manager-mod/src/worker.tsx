import type { EngineInterface, PromptSubmitInput, RenderInput, RenderViewport, SessionAppendInput, SiteView, UiScrollInput } from 'claude-code'

import type { DisplayRow, Model, Page, PageEntry, WorkerView } from '../types'

import { cells, cut } from './cells'
import { askLine, doing, entriesOf, entriesOfFile, inRoom, noBrief, ownDropBlocks, pageRows, pendingAfter, briefNote, briefWith, startLine } from './page'
import { changePage, readPage, readPageRow, readWorkerView, readView, say, writePage, writePaged, writePageRow, writeWorkerView } from './state'

// What a press on a row opens: a page, as Giverny's worker page is (docs/management-panel.md, What a
// click on a row opens). In fullscreen the page takes the transcript's place: a placed row draws it
// and every other row of the transcript draws nothing while it is open, so nothing else on the
// screen moves. With `worker_view` at `claude`, or on the main screen, where what is drawn cannot be
// taken back, a worker Claude Code still lists opens in its own view of it, `$.ui.view`, whose prompt
// is that worker's and which adds `○ main` under the prompt; there, every other row's page is a pane.

export const PANE = 'worker'
export const PAGE_ROW = 'manager-page'
/** What a transcript is made of, all a hook can draw: the logo is not among them, and stays above a page as above a view. */
export const TRANSCRIPT_ROWS = ['UserMessage', 'AssistantMessage', 'ToolUse', 'ToolResult', 'ToolGroup', 'ToolProgress', 'CommandOutput', 'Spinner', 'TurnDuration', 'InfoNotice'] as const
const FOLLOW_MS = 1000
/** What `$.fs.read` takes, and what is read of a file longer than that. */
const READ_MOST = 4 * 1024 * 1024
const TAIL_BYTES = 2 * 1024 * 1024
const STORE_VIEW = 'worker-view'
/** What the manifest said when `/manager view` last spoke: a change in /config beats that word. */
const STORE_VIEW_CONFIG = 'worker-view-config'
const LAID_OUT_MS = [150, 450]
/** Long enough for the engine to have put a dropped line back in the box and kept its row. */
const AFTER_DROP_MS = 30
const DROP_MOST = 200
// Claude Code's own prompt band, as it draws one in a worker's view.
const PROMPT_BG = 'ansi256(237)'

/** While the person has scrolled up, the page stays where they put it. */
let pinned = false
let reading = false
let fileMs: number | undefined
/** The reason of the drop just answered, until its row comes. */
let dropped: string | undefined

const isWorkerView = (v: unknown): v is WorkerView => v === 'page' || v === 'claude'

/** The manifest's field: anything but `claude` is the page. */
export const configuredView = (options: Readonly<Record<string, unknown>>): WorkerView => (options.worker_view === 'claude' ? 'claude' : 'page')

/** The word `/manager view` left, unless /config has changed since. A store that cannot be read: the manifest's. */
export async function loadView($: EngineInterface, configured: WorkerView): Promise<void> {
  try {
    if ((await $.store.get(STORE_VIEW_CONFIG)) !== configured) {
      await $.store.delete(STORE_VIEW)
      await $.store.set(STORE_VIEW_CONFIG, configured)
    }
    const kept = await $.store.get(STORE_VIEW)
    await writeWorkerView($, isWorkerView(kept) ? kept : configured)
  } catch {
    await writeWorkerView($, configured)
  }
}

/** Whether a worker's row opens the page here, not Claude Code's view: the band then keeps no rows for that view. */
export function pagesWorkers(mode: WorkerView, screen: RenderViewport | undefined): boolean {
  return mode === 'page' && screen?.isFullscreen === true
}

/** `/manager view page|claude`, as said back. What is open goes, so the band and the screen agree at once. */
export async function viewCommand($: EngineInterface, arg: string): Promise<string> {
  const now = await readWorkerView($)
  if (!isWorkerView(arg)) return `Usage: /manager view page|claude. Now ${now}.`
  await writeWorkerView($, arg)
  await $.store.set(STORE_VIEW, arg)
  if (arg !== now) await goBack($)
  return arg === 'page'
    ? "A worker's row opens the plugin's page in fullscreen: no row or prompt moves."
    : "A worker's row opens Claude Code's own view: 2 rows are kept under the table for what it adds under the prompt."
}

/** A press on a row: its page, or its worker's view; on what is open, back to the manager. */
export async function openRow($: EngineInterface, r: DisplayRow, view: SiteView | undefined, band: string, screen: RenderViewport | undefined): Promise<void> {
  const page = await readPage($)
  const onIt = page?.rowId === r.id || (r.workerId !== undefined && view?.agentId === r.workerId)
  if (onIt) return goBack($, view)
  if (r.workerId !== undefined && !pagesWorkers(await readWorkerView($), screen)) {
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

/**
 * A row of its own at the transcript's end for a page: under what has come since the last. A row
 * Claude Code adds under the page while it is open stays there until the next.
 */
async function placeRow($: EngineInterface) {
  const id = `${PAGE_ROW}-${await $.clock.now()}`
  return { id, ...(await $.ui.place({ id })) }
}

async function openPage($: EngineInterface, r: DisplayRow, band: string, screen: RenderViewport | undefined): Promise<void> {
  const ask = r.own === 'next' ? ('start' as const) : r.workerId !== undefined ? ('worker' as const) : undefined
  const placed = screen?.isFullscreen === true ? await placeRow($) : undefined
  pinned = false
  fileMs = undefined
  const page: Page = { place: placed?.isPlaced ? 'row' : 'pane', band, rowId: r.id, row: r, entries: await firstEntries($, r), ask, pending: [], prompts: 0, status: '', draft: '', box: 0, movedMs: fileMs ?? (await $.clock.now()) }
  page.prompts = page.entries.filter(e => e.kind === 'prompt').length
  await writePage($, page)
  await say($, '')
  if (placed?.isPlaced) {
    await writePageRow($, placed.id)
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

/** The lines added to a Next up row with no brief file to hold them, for its page. */
const added = new Map<string, string[]>()

async function firstEntries($: EngineInterface, r: DisplayRow): Promise<PageEntry[]> {
  if (r.own === 'next' && r.brief !== undefined) {
    try {
      return [{ kind: 'prompt', text: await $.fs.read(r.brief) }]
    } catch (e) {
      return [{ kind: 'tool', text: `Its brief ${r.brief} cannot be read: ${e instanceof Error ? e.message : String(e)}`, isError: true }]
    }
  }
  if (r.own === 'next') return [{ kind: 'prompt', text: noBrief(r.key, r.title, r.note) }, ...(added.get(r.id) ?? []).map(text => ({ kind: 'prompt' as const, text: `The user adds: ${text}` }))]
  const read = await readEntries($, r)
  if (read && read.length > 0) return read
  const lines = [r.key && `Task: ${r.key}`, r.title !== r.key ? r.title : '', r.note ?? ''].filter(Boolean).join('\n\n')
  const out: PageEntry[] = [{ kind: 'prompt', text: lines || 'This row has no worker and no transcript.' }]
  for (const w of r.why) out.push({ kind: 'tool', text: w })
  // Giverny runs a row's `open` in a new tab; a plugin has no tabs to open.
  if (r.open !== undefined) out.push({ kind: 'tool', text: `To open it: ${r.open}` })
  return out
}

/** A worker's entries: its transcript file, else what the session holds of it. */
async function readEntries($: EngineInterface, r: DisplayRow): Promise<PageEntry[] | undefined> {
  // The file first, read only when it moved: it alone has the lines that reached a worker in the
  // middle of its turn, and a long transcript read once a second is a cost of its own.
  if (r.transcript !== undefined) {
    try {
      const { mtimeMs, size } = await $.fs.stat(r.transcript)
      if (mtimeMs === fileMs) return undefined
      const whole = size <= READ_MOST
      // `$.fs.read` refuses a file past 4 MiB. A page draws far less than this tail holds.
      const text = whole ? await $.fs.read(r.transcript) : (await $.process.run(['tail', '-c', String(TAIL_BYTES), r.transcript])).stdout
      fileMs = mtimeMs
      const entries = entriesOfFile(text)
      return whole ? entries : [{ kind: 'tool', text: `The start of its transcript is not drawn: ${Math.round(size / 1048576)} MB in all, at ${r.transcript}` }, ...entries]
    } catch {
      // What the session holds of it, then.
    }
  }
  if (r.workerId === undefined) return undefined
  const found = await $.session.messages({ agentId: r.workerId })
  return 'deny' in found ? undefined : entriesOf(found)
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

/** What a page draws of its row, and what the page needs to find its worker. */
const shownOfRow = (r: DisplayRow) => JSON.stringify([r.key, r.title, r.review, r.own, r.what, r.finished, r.workerId, r.transcript, r.own === 'running' ? [] : r.facts])

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
    // The model's row for what the page draws of it. The model changes every poll as its clocks tick,
    // so only a change a page shows reaches the page and redraws its long tree.
    const row = (await readView($)).rows.find(one => one.id === page.rowId) ?? page.row
    if (shownOfRow(row) !== shownOfRow(page.row)) await changePage($, now => ({ ...now, row }))
    const entries = await readEntries($, row)
    if (!entries || JSON.stringify(entries) === JSON.stringify(page.entries)) return
    const movedMs = await $.clock.now()
    await changePage($, now => {
      const pending = pendingAfter(now.pending, now.prompts, entries)
      const prompts = pending.length < now.pending.length ? entries.filter(e => e.kind === 'prompt').length : now.prompts
      return { ...now, entries, pending, prompts, movedMs }
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
  await writePageRow($, null)
  if (page?.place === 'pane') await $.ui.close({ id: PANE })
}

/** A Next up row as it stands now: the page holds it as it was when it opened. */
async function stillNext($: EngineInterface, page: Page): Promise<boolean> {
  return (await readView($)).rows.find(one => one.id === page.rowId)?.own === 'next'
}

/**
 * A line typed on a Next up row: nobody is there to read it yet, so it joins the row's brief, and the
 * manager reads that at its next turn, as it may have the worker's prompt ready. No turn goes on it.
 */
async function addToBrief($: EngineInterface, page: Page, text: string): Promise<{ sent: boolean; status: string }> {
  const r = page.row
  try {
    if (!(await stillNext($, page))) return { sent: false, status: 'Not sent: this task has left Next up. Go back and open its row.' }
    if (r.brief !== undefined) await $.fs.write(r.brief, briefWith(await $.fs.read(r.brief), text))
    else added.set(r.id, [...(added.get(r.id) ?? []), text])
    const told = await $.session.append({ message: { type: 'user', content: [{ type: 'text', text: briefNote(r.key || r.title, r.brief, text) }] } })
    const entries = await firstEntries($, r)
    await changePage($, now => ({ ...now, entries }))
    return 'deny' in told && told.deny !== undefined
      ? { sent: true, status: `Added to this task's brief. The manager was not told: ${told.deny}` }
      : { sent: true, status: "Added to this task's brief." }
  } catch (e) {
    return { sent: false, status: `Not sent: ${e instanceof Error ? e.message : String(e)}` }
  }
}

/** The start button on a Next up row's page: the manager claims and spawns, so it is asked, and the page gives way to it. */
export async function startFromPage($: EngineInterface, view?: SiteView): Promise<void> {
  const page = await readPage($)
  if (page?.ask !== 'start') return
  if (!(await stillNext($, page))) {
    await changePage($, now => ({ ...now, status: 'Not sent: this task has left Next up. Go back and open its row.' }))
    return
  }
  await goBack($, view)
  await $.prompt.submit({ text: startLine(page.row.key), asUser: true })
}

/** A line for the row's worker, or for a Next up row, its brief: the person's line only. What it came to, and whether it went. */
async function deliver($: EngineInterface, page: Page, text: string): Promise<{ sent: boolean; status: string }> {
  if (page.ask === 'worker' && page.row.workerId !== undefined) {
    const r = await $.session.send({ to: { agentId: page.row.workerId }, text })
    return { sent: r.isDelivered, status: r.isDelivered ? 'Sent to this worker.' : `Not sent: ${r.reason}` }
  }
  return addToBrief($, page, text)
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
  if (page.ask === undefined) return ownDrop($, e, 'This page is read-only: press 0 to go back to the manager first.', false)
  const { sent, status } = await deliver($, page, e.text)
  await changePage($, now => ({ ...now, status, pending: sent && page.ask === 'worker' ? [...now.pending, e.text] : now.pending }))
  const name = page.row.key || page.row.title || 'this worker'
  return ownDrop($, e, status, sent, sent ? (page.ask === 'start' ? `Added to ${name}'s brief: ${e.text}` : `Sent to ${name}: ${e.text}`) : undefined)
}

/**
 * A plugin takes a line from the prompt only by dropping it. Over a running turn of the manager's,
 * Claude Code says so for a moment in its notification line, `Prompt dropped by a hook: <reason>`,
 * out of any hook's reach. With none running it keeps a row saying that in the manager's transcript,
 * which no hook can remove and which reads `(no content)` with no words: ownDropWords gives it
 * what happened, and the page goes back under it. Either way the line comes back to the box after
 * this answers: taken out once it has if it went, and left there to try again if not.
 */
async function ownDrop($: EngineInterface, e: PromptSubmitInput, status: string, sent: boolean, record?: string): Promise<{ drop: string }> {
  const oneLine = (text: string) => cut(text.split(/\s*\n\s*/).join(' '), DROP_MOST)
  const said = oneLine(status)
  const reason = oneLine(record ?? status)
  dropped = e.turnId === undefined ? reason : undefined
  await say($, said)
  $.clock.after(AFTER_DROP_MS, async () => {
    if (sent) await $.prompt.fill({ text: '' }).catch(() => {})
    if (e.turnId === undefined) await underDrop($).catch(() => {})
  })
  return { drop: reason }
}

async function underDrop($: EngineInterface): Promise<void> {
  if ((await readPage($))?.place !== 'row') return
  const placed = await placeRow($)
  if (placed.isPlaced) await writePageRow($, placed.id)
}

/** The row Claude Code keeps for a drop of ours, in the words of what happened. */
export function ownDropWords(e: SessionAppendInput): SessionAppendInput {
  if (dropped === undefined || e.message.type !== 'system') return e
  const content = ownDropBlocks(e.message.content, dropped)
  if (!content) return e
  dropped = undefined
  return { ...e, message: { ...e.message, content } } as SessionAppendInput
}

/** A transcript's row while a page is in its place. */
export function hiddenRow($: EngineInterface, e: RenderInput) {
  const { Box } = $.ui.resolve(e)
  return <Box display="none" />
}

/** A placed row, which stands in the transcript for good: the open page's, or nothing. */
export async function drawPageRow($: EngineInterface, e: RenderInput<'Placed'>) {
  return (await readPageRow($)) === e.props.id ? drawPage($, e) : hiddenRow($, e)
}

/** The page's tree. */
export async function drawPage($: EngineInterface, e: RenderInput<'Pane'> | RenderInput<'Placed'>) {
  const { Box, Text, Button, Markdown, Client } = $.ui.resolve(e)
  const page = await readPage($)
  if (!page) return <Text dimColor>Nothing open.</Text>
  const r = page.row
  const { shown, left } = inRoom(page.entries)
  const entry = (one: PageEntry) => {
    if (one.kind === 'prompt') {
      return (
        <Box backgroundColor={PROMPT_BG} paddingRight={1}>
          <Text wrap="wrap">{`❯ ${one.text}`}</Text>
        </Box>
      )
    }
    if (one.kind === 'tool') {
      return (
        <Box flexDirection="column">
          <Text color={one.isError && one.result === undefined ? 'red' : undefined} dimColor={!one.isError || one.result !== undefined} wrap="truncate">{`  ${one.text}`}</Text>
          {one.result !== undefined && <Text color={one.isError ? 'red' : undefined} dimColor={!one.isError} wrap="truncate">{`  ⎿ ${one.result}`}</Text>}
        </Box>
      )
    }
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
    <Box flexDirection="column" gap={1} paddingTop={page.place === 'row' ? 1 : 0}>
      <Box flexDirection="column">
        <Box flexDirection="row" gap={2}>
          <Text bold>{r.key || r.title}</Text>
          {page.place === 'pane' && (
            <Button key="back" hotkey="0" plain onPress={() => goBack($)}>
              back to the manager
            </Button>
          )}
          {page.place === 'pane' && page.ask === 'start' && (
            <Button key="start-now" plain onPress={() => startFromPage($)}>
              start now
            </Button>
          )}
        </Box>
        {r.key !== '' && r.title !== r.key && <Text dimColor>{r.title}</Text>}
        {/* A running row's facts tick with its clocks: its row in the band, right there, has them. */}
        {r.own !== 'running' && r.facts.length > 0 && <Text dimColor>{r.facts.join(' · ')}</Text>}
        {r.review !== undefined && <Text color="yellow">{`Review: ${r.review}`}</Text>}
      </Box>
      {left > 0 && <Text dimColor>{`${left} earlier entries are not drawn: a pane draws so much text.`}</Text>}
      {shown.map(entry)}
      {page.pending.map(line => (
        <Text dimColor wrap="wrap">{`❯ ${line}`}</Text>
      ))}
      {page.ask === undefined && <Text dimColor>read-only</Text>}
      {page.place === 'row' && page.status.startsWith('Not sent') && <Text color="red" wrap="wrap">{page.status}</Text>}
      {r.own === 'running' && r.workerId !== undefined && !r.finished && (
        <Client key={`doing-${page.movedMs ?? 0}`} module="./doing-client.tsx" props={{ words: doing(page.entries, r.what), since: Math.max(0, Math.floor(((await $.clock.now()) - (page.movedMs ?? 0)) / 1000)), ink: 'ansi256(32)' }} width={Math.max(1, (e.viewport?.columns ?? 80) - 2)} height={1} />
      )}
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
  const line = page?.place !== 'row' ? '' : page.ask === 'start' ? "a line at the prompt joins this task's brief" : page.ask === 'worker' ? 'a line at the prompt goes to this worker' : 'read-only'
  const ways = ['or press 0 at the empty prompt, or click its row again', 'or press 0, or click its row again', 'or press 0']
  const tries = ways.flatMap(way => [
    [line, way, esc && `Esc at the prompt stops ${esc}`],
    [line, way, esc && `Esc stops ${esc}`],
  ]).concat([[way0(line), 'or press 0', esc && `Esc stops ${esc}`], ['or press 0', esc && 'Esc stops it'], [esc ? 'Esc stops it' : 'or press 0']])
  const fit = tries.map(t => t.filter(Boolean).join('  ·  ')).find(t => cells(t) <= room - (said ? cells(said) + 5 : 0)) ?? ''
  return said ? [said, fit].filter(Boolean).join('  ·  ') : fit
}

const way0 = (line: string) => (line === 'read-only' ? line : '')
