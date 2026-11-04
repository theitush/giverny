import type { Load, Section } from '../types'

// The colours of Giverny's panel, by its own names, as raw hex so the band reads as the panel
// whatever Claude Code's theme is. crates/app/src/management_panel.rs RUNNING, PLANNED, DONE;
// crates/term/src/render/theme.rs and chrome.rs for the rest, which the panel takes from its
// theme: `ink`, Ita's, on a dark terminal, and `monet-light` on a light one.

/** The three stage tints, the 256-colour codes 38;5;32, 38;5;67 and 38;5;28: they read on dark and light alike. */
export const STAGE: Record<Section, string> = { running: '#0087d7', next: '#5f87af', done: '#008700' }

/**
 * The same three as what is painted: the codes themselves. A hex goes out rounded on a terminal of
 * 256 colours (tmux's default: `#0087d7` came out as 38;5;38, a cyan), and the codes are the panel's.
 */
export const STAGE_INK: Record<Section, string> = { running: 'ansi256(32)', next: 'ansi256(67)', done: 'ansi256(28)' }

export type Chrome = {
  /** What the terminal is painted in: under a tinted row, and on the back button's words. */
  bg: string
  /** A missing ETA, the footer, what is said beside the back button. */
  dim: string
  /** A flagged NOW, an OOM, a busy figure. */
  amber: string
  /** A heavy figure. */
  poppy: string
  /** The back button's fill. */
  accent: string
  /** The rule along the table's top, where egui draws the panel's separator. */
  rule: string
  /** A row's hover, as egui's tooltip: its window's fill and its text. */
  tip: string
  tipInk: string
}

export const DARK: Chrome = { bg: '#0b0b0c', dim: '#838384', amber: '#f0dc9a', poppy: '#e8897a', accent: '#96d2d2', rule: '#3c3c3c', tip: '#1b1b1b', tipInk: '#a0a0a0' }
export const LIGHT: Chrome = { bg: '#f7f4ec', dim: '#7f8180', amber: '#9a7418', poppy: '#a8412f', accent: '#2c7d7d', rule: '#bebebe', tip: '#f8f8f8', tipInk: '#505050' }

/** Claude Code's `theme` setting into the panel's chrome. `auto` cannot be told: dark. */
export const chromeOf = (theme: unknown): Chrome => (typeof theme === 'string' && theme.includes('light') ? LIGHT : DARK)

/** How strongly the row on view is lit, and a row under the pointer: VIEWED_TINT and row_tint. */
export const VIEWED_TINT = 0.26
export const HOVER_TINT = 0.1

const channels = (hex: string) => [1, 3, 5].map(at => parseInt(hex.slice(at, at + 2), 16))

/** A colour at `alpha` over `bg`, as egui paints `gamma_multiply`: a terminal cell has no alpha. */
export function tint(color: string, bg: string, alpha: number): string {
  const under = channels(bg)
  return `#${channels(color)
    .map((c, i) => Math.round(c * alpha + under[i]! * (1 - alpha)).toString(16).padStart(2, '0'))
    .join('')}`
}

/** A figure by its weight on the machine, as `Load::color`: light keeps its row's colour. */
export const loadInk = (load: Load, row: string, c: Chrome) => (load === 'heavy' ? c.poppy : load === 'busy' ? c.amber : row)
