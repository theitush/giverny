// Terminal cells, not characters: what the band's columns are counted in.

const SEGMENTER = new Intl.Segmenter(undefined, { granularity: 'grapheme' })
const WIDE = /[ᄀ-ᅟ⺀-꓏가-힣豈-﫿︰-﹯＀-｠￠-￦]|\p{Emoji_Presentation}/u

const graphemes = (s: string) => Array.from(SEGMENTER.segment(s), g => g.segment)
const cellsOf = (g: string) => (WIDE.test(g) ? 2 : /^\p{M}*$/u.test(g) ? 0 : 1)

export const cells = (s: string) => graphemes(s).reduce((n, g) => n + cellsOf(g), 0)

/** The start of `s`, at most `n` cells of it, the last an ellipsis if anything was cut. */
export function cut(s: string, n: number): string {
  if (n <= 0) return ''
  if (cells(s) <= n) return s
  let out = ''
  let used = 0
  for (const g of graphemes(s)) {
    if (used + cellsOf(g) > n - 1) break
    out += g
    used += cellsOf(g)
  }
  return `${out.trimEnd()}…`
}

/** A key cut from the middle: `task-52…-mod`. Both ends of a key tell keys apart. */
export function cutMiddle(s: string, n: number): string {
  if (cells(s) <= n) return s
  if (n < 5) return cut(s, n)
  const gs = graphemes(s)
  const tail = Math.floor((n - 1) / 2)
  return `${gs.slice(0, n - 1 - tail).join('')}…${gs.slice(-tail).join('')}`
}

export const padEnd = (s: string, n: number) => s + ' '.repeat(Math.max(0, n - cells(s)))
export const padStart = (s: string, n: number) => ' '.repeat(Math.max(0, n - cells(s))) + s

/** The start of `s`, at most `n` cells of it, the last an ellipsis if anything was cut; as the panel cuts, spaces kept. */
export function cutExact(s: string, n: number): string {
  if (n <= 0) return ''
  if (cells(s) <= n) return s
  let out = ''
  let used = 0
  for (const g of graphemes(s)) {
    if (used + cellsOf(g) > n - 1) break
    out += g
    used += cellsOf(g)
  }
  return `${out}…`
}

/** `s` as one entry a cell; a wide grapheme's second cell is ''. */
export function cellsIn(s: string): string[] {
  return graphemes(s).flatMap(g => (cellsOf(g) === 2 ? [g, ''] : cellsOf(g) === 1 ? [g] : []))
}
