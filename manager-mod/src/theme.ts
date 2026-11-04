import type { Color } from 'claude-code'

import type { DisplayRow } from '../types'

import { KILLED } from './model'

// What the band's colours and marks mean, in one place. Every colour is a theme key, so the band
// follows the person's theme, light, dark or ansi, and no raw colour can go unreadable on one.
// Five inks, each one meaning: anything not worth a person's attention is dim, and has no ink.

export const INK = {
  /** Under way, inside its estimate. */
  moving: 'ide',
  /** Past its estimate, or something the person should know. */
  behind: 'warning',
  /** Landed. */
  landed: 'success',
  /** Landed, and waits for a person's eye. */
  eye: 'merged',
  /** Stopped for good, blocked or killed: it will not get better by itself. */
  failed: 'error',
} as const satisfies Record<string, Color>

/** A row's mark: one cell, in the gutter Claude's own bullets use. */
export type Mark = { glyph: string; color?: Color; dim?: boolean }

const MOVING: Mark = { glyph: '●', color: INK.moving }
const BEHIND: Mark = { glyph: '●', color: INK.behind }
const STILL: Mark = { glyph: '‖', dim: true }
const STUCK: Mark = { glyph: '‖', color: INK.behind }
const IDLE: Mark = { glyph: '◌', dim: true }
const READY: Mark = { glyph: '○', dim: true }
const LANDED: Mark = { glyph: '✓', color: INK.landed }
const EYE: Mark = { glyph: '◆', color: INK.eye }
const FAILED: Mark = { glyph: '×', color: INK.failed }

const begins = (s: string, words: string[]) => words.some(w => s.toLowerCase().startsWith(w))

/** Does this Done row wait for a person? Its landing says so (`Review — ita`). */
export const wantsEye = (r: DisplayRow) => r.own === 'done' && (r.review !== undefined || begins(r.what, ['review']))

/** The mark says in one cell what the row's words say; it goes by the row's own stage, not its place. */
export function markOf(r: DisplayRow): Mark {
  const what = r.what
  if (r.own === 'running') {
    if (r.oom || what === KILLED) return FAILED
    if (r.flag || begins(what, ['stopped'])) return STUCK
    if (begins(what, ['paused']) || / limit( →|$)/.test(what)) return STILL
    if (r.asleep || r.finished) return IDLE
    return r.over ? BEHIND : MOVING
  }
  if (r.own === 'next') {
    if (begins(what, ['held', 'paused'])) return STILL
    return what === '' ? READY : IDLE
  }
  if (wantsEye(r)) return EYE
  if (r.oom || begins(what, ['blocked', 'killed', 'stopped', 'failed', 'cancelled'])) return FAILED
  return LANDED
}

/** A section's mark where no row stands for it: the header's short form, a section's summary line. */
export const SECTION_MARK = { running: MOVING, next: READY, done: LANDED } as const

export const GAUGE = { full: '━', empty: '─', notch: '╋' } as const
