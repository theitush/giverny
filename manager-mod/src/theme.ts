import type { Color } from 'claude-code'

import type { DisplayRow } from '../types'

// What the band's colours and marks mean, in one place. Every colour is a theme key, so the band
// follows the person's theme, light, dark or ansi, and no raw colour can go unreadable on one.
// Five inks, each one meaning: anything not worth a person's attention is dim, and has no ink.

export const INK = {
  /** Under way, inside its estimate. */
  moving: 'ide',
  /** Past its estimate: look at it. */
  behind: 'warning',
  /** Landed. */
  landed: 'success',
  /** Landed, and waits for a person's eye. */
  eye: 'merged',
  /** Stopped, blocked or killed: it will not get better by itself. */
  failed: 'error',
} as const satisfies Record<string, Color>

/** A row's mark: one cell, in the gutter Claude's own bullets use. */
export type Mark = { glyph: string; color?: Color; dim?: boolean }

const MOVING: Mark = { glyph: '●', color: INK.moving }
const BEHIND: Mark = { glyph: '●', color: INK.behind }
const STILL: Mark = { glyph: '‖', dim: true }
const ASLEEP: Mark = { glyph: '◌', dim: true }
const READY: Mark = { glyph: '○', dim: true }
const LANDED: Mark = { glyph: '✓', color: INK.landed }
const EYE: Mark = { glyph: '◆', color: INK.eye }
const FAILED: Mark = { glyph: '✕', color: INK.failed }

const startsWithAny = (s: string, words: string[]) => words.some(w => s.toLowerCase().startsWith(w))

/** Does this Done row wait for a person? Its landing says so (`Review — ita`). */
export const wantsEye = (r: DisplayRow) => r.section === 'done' && r.what.toLowerCase().startsWith('review')

/**
 * The mark says what the row's words say, read from the words the model hands over, so it holds
 * for any row the model can draw. Words it does not know read as the section's plain mark.
 */
export function markOf(r: DisplayRow): Mark {
  const what = r.what
  if (r.section === 'running') {
    if (startsWithAny(what, ['killed', 'stopped', 'oom'])) return FAILED
    if (r.left === 'paused' || startsWithAny(what, ['paused'])) return STILL
    if (startsWithAny(what, ['asleep', 'waiting'])) return ASLEEP
    return r.over ? BEHIND : MOVING
  }
  if (r.section === 'next') {
    if (startsWithAny(what, ['held'])) return STILL
    if (what !== '') return ASLEEP
    return READY
  }
  if (wantsEye(r)) return EYE
  if (startsWithAny(what, ['blocked', 'killed', 'stopped', 'failed', 'oom', 'cancelled'])) return FAILED
  return LANDED
}

/** The header's mark for a section, as its rows draw it. */
export const SECTION_MARK = { running: MOVING, next: READY, done: LANDED } as const

export const GAUGE = { full: '━', empty: '─', notch: '╋' } as const
