import { atom, read, update } from 'claude-code'
import type { EngineInterface } from 'claude-code'

import type { Model, Page } from '../types'

// What a drawing reads. The validator wants an atom used in the file that declares it, so every
// read and write goes through a function here (a declaration: past an import the validator
// follows `$` only into `function name($)`). A write redraws the readers; reading while
// drawing subscribes the drawing. Add a value here, in types/index.d.ts, and an accessor.
const EMPTY: Model = { rows: [], running: 0, next: 0, done: 0, nested: false }

const view = atom({ plugin: 'manager', key: 'view' } as const, EMPTY, { shape: 'view-2' })
const hidden = atom({ plugin: 'manager', key: 'hidden' } as const, false)
const said = atom({ plugin: 'manager', key: 'said' } as const, '')
const NONE: string[] = []
const folded = atom({ plugin: 'manager', key: 'folded' } as const, NONE, { shape: 'folded-1' })
const NOT_CLEARED: { atMs?: number; agents: string[] } = { agents: [] }
const cleared = atom({ plugin: 'manager', key: 'cleared' } as const, NOT_CLEARED, { shape: 'cleared-1' })
const NO_PAGE: Page | null = null
const page = atom({ plugin: 'manager', key: 'page' } as const, NO_PAGE, { shape: 'page-2' })
/** Whether a page is on, which every row of the transcript reads: it changes only as one opens or closes. */
const paged = atom({ plugin: 'manager', key: 'paged' } as const, false)
/** The rows the person set the band to, under its rule; null: it follows its rows. */
const NOT_SET: number | null = null
const size = atom({ plugin: 'manager', key: 'size' } as const, NOT_SET, { shape: 'size-1' })
/** The band's first row on show, when it shows fewer than it has. */
const top = atom({ plugin: 'manager', key: 'top' } as const, 0)

export function readView($: EngineInterface) {
  return read($, view)
}
export function writeView($: EngineInterface, model: Model) {
  return update($, view, () => model)
}
export function readHidden($: EngineInterface) {
  return read($, hidden)
}
export function toggleHidden($: EngineInterface) {
  return update($, hidden, h => !h)
}
export function readSaid($: EngineInterface) {
  return read($, said)
}
export function say($: EngineInterface, words: string) {
  return update($, said, () => words)
}
export function readFolded($: EngineInterface) {
  return read($, folded)
}
/** Fold or show the agents under a row (DisplayRow.fold.id). */
export function toggleFold($: EngineInterface, id: string) {
  return update($, folded, now => (now.includes(id) ? now.filter(f => f !== id) : [...now, id]))
}
export function readCleared($: EngineInterface) {
  return read($, cleared)
}
export function writeCleared($: EngineInterface, value: { atMs: number; agents: string[] }) {
  return update($, cleared, () => value)
}
export function readPage($: EngineInterface): Promise<Page | null> {
  return read($, page)
}
export function writePage($: EngineInterface, value: Page | undefined) {
  return update($, page, () => value ?? null)
}
/** A change to the open page, on what it holds by then: a line and a look at the file can cross. */
export function changePage($: EngineInterface, change: (now: Page) => Page) {
  return update($, page, now => (now ? change(now) : now))
}
export function readPaged($: EngineInterface) {
  return read($, paged)
}
export function writePaged($: EngineInterface, value: boolean) {
  return update($, paged, () => value)
}
export function readSize($: EngineInterface): Promise<number | null> {
  return read($, size)
}
export function writeSize($: EngineInterface, value: number | null) {
  return update($, size, () => value)
}
export function readTop($: EngineInterface) {
  return read($, top)
}
export function writeTop($: EngineInterface, value: number) {
  return update($, top, () => value)
}
