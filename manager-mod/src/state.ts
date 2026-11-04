import { atom, read, update } from 'claude-code'
import type { EngineInterface } from 'claude-code'

import type { Model } from '../types'

// What a drawing reads. The validator wants an atom used in the file that declares it, so every
// read and write goes through a function here (a declaration: past an import the validator
// follows `$` only into `function name($)`). A write redraws the readers; reading while
// drawing subscribes the drawing. Add a value here, in types/index.d.ts, and an accessor.
const EMPTY: Model = { rows: [], running: 0, next: 0, done: 0 }

const view = atom({ plugin: 'manager', key: 'view' } as const, EMPTY, { shape: 'view-1' })
const hidden = atom({ plugin: 'manager', key: 'hidden' } as const, false)
const said = atom({ plugin: 'manager', key: 'said' } as const, '')

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
