import type { Hold, Limit, Span } from '../types'

// The panel's words for time: crates/claude/src/feed.rs fmt_span and fmt_delta,
// crates/app/src/management_panel.rs countdown and clock_at.

/** A span as the clock columns write it: `5m`, `1h3m`, `1d3h12m`, whole minutes, `0m` for nothing. */
export function fmtSpan(seconds: number): string {
  const secs = Math.trunc(seconds)
  const mins = Math.floor((Math.abs(secs) + 30) / 60)
  const units: [number, string][] = [
    [Math.floor(mins / 1440), 'd'],
    [Math.floor((mins % 1440) / 60), 'h'],
    [mins % 60, 'm'],
  ]
  const body = units.flatMap(([n, u]) => (n === 0 ? [] : [`${n}${u}`])).join('') || '0m'
  return secs < 0 && mins !== 0 ? `-${body}` : body
}

/** ELAPSED: `9:40`, `1:05:00`. */
export function stopwatch(seconds: number): string {
  const secs = Math.max(0, Math.trunc(seconds))
  const [h, m, s] = [Math.floor(secs / 3600), Math.floor((secs % 3600) / 60), secs % 60]
  const two = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`
}

/** A Done row's ETA cell: `(+5m)` late, `(-1h20m)` early, `(±0m)` on the minute. */
export function fmtDelta(seconds: number): string {
  const s = fmtSpan(seconds)
  if (s === '0m') return '(±0m)'
  return seconds > 0 ? `(+${s})` : `(${s})`
}

/** A Running row's time left: `~20m`, and `-~12m` once past its estimate. */
export function countdown(leftS: number): string {
  if (leftS < 0 && fmtSpan(leftS) !== '0m') return `-~${fmtSpan(-leftS)}`
  return `~${fmtSpan(Math.max(leftS, 0))}`
}

/** Claude Code's compact count: `842`, `13.5k`, `1.2M`. */
export function fmtTokens(n: number): string {
  const steps: [number, number, string][] = [
    [999_950_000, 1_000_000_000, 'B'],
    [999_950, 1_000_000, 'M'],
    [1000, 1000, 'k'],
  ]
  for (const [floor, div, suffix] of steps) {
    if (n >= floor) return `${(n / div).toFixed(1).replace(/\.0$/, '')}${suffix}`
  }
  return String(n)
}

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
const pad = (n: number) => String(n).padStart(2, '0')

/** `13:00` for an instant today, `Mon 08:00` for one further off, in the machine's own zone. */
export function clockAt(atMs: number, nowMs: number): string {
  const at = new Date(atMs)
  const now = new Date(nowMs)
  const hm = `${pad(at.getHours())}:${pad(at.getMinutes())}`
  const today = at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth() && at.getDate() === now.getDate()
  return today ? hm : `${DAYS[at.getDay()]} ${hm}`
}

/** The start of `now`'s minute: what every clock on the band counts from, so all tick at once. */
export const minuteOf = (now: number) => Math.floor(now / 60000) * 60000

/** Milliseconds of `[start, upto]` inside any of `spans`, overlaps counted once. */
export function offMs(spans: Span[], start: number, upto: number): number {
  const cut = spans
    .map(s => [Math.max(s.since, start), Math.min(s.until, upto)] as const)
    .filter(([a, b]) => b > a)
    .sort((x, y) => x[0] - y[0] || x[1] - y[1])
  let total = 0
  let reach = start
  for (const [from, to] of cut) {
    const a = Math.max(from, reach)
    if (to > a) {
      total += to - a
      reach = to
    }
  }
  return total
}

const WINDOWS: Record<string, string> = { five_hour: '5h', seven_day: '7d' }

/** The window that is used up, with a reset still to come; the one reopening last when two are. */
export function spent(windows: { kind: string; percentUsed: number; resetsAt?: string }[], now: number): Limit | undefined {
  let found: Limit | undefined
  for (const w of windows) {
    const short = WINDOWS[w.kind]
    const at = w.resetsAt === undefined ? Number.NaN : Date.parse(w.resetsAt)
    if (!short || w.percentUsed < 100 || Number.isNaN(at) || at <= now) continue
    if (found?.reopensMs === undefined || at > found.reopensMs) found = { window: short, reopensMs: at }
  }
  return found
}

const MAX_HOLDS = 32

/** Open a hold when a limit is first seen out, close it when it is gone, at its reset if it named one. */
export function track(holds: Hold[], limit: Limit | undefined, now: number): void {
  const out = limit && (limit.reopensMs === undefined || limit.reopensMs > now) ? limit : undefined
  const last = holds.at(-1)
  const open = last && last.untilMs === undefined ? last : undefined
  if (out && open) open.reopensMs = out.reopensMs ?? open.reopensMs
  else if (out) {
    holds.push({ sinceMs: now, reopensMs: out.reopensMs })
    if (holds.length > MAX_HOLDS) holds.shift()
  } else if (open) {
    const end = open.reopensMs === undefined ? now : Math.min(open.reopensMs, now)
    open.untilMs = Math.max(end, open.sinceMs)
  }
}
