import type { FeedRow } from '../types'

/** Seconds as the band says them: `<1m`, `12m`, `1h15`. */
export function dur(seconds: number): string {
  const s = Math.max(0, Math.round(seconds))
  if (s < 60) return '<1m'
  const m = Math.round(s / 60)
  if (m < 60) return `${m}m`
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}`
}

/** The start of `now`'s minute: what every clock on the band counts from, so all tick at once. */
export const minuteOf = (now: number) => Math.floor(now / 60000) * 60000

/** How long a running row has run; a paused one holds where it stopped. */
export function elapsed(r: FeedRow, now: number): string {
  if (r.started === undefined) return ''
  return dur(((r.paused_since ?? now) - r.started) / 1000)
}

/** `~8m left`, or `over by 3m` once the figure has run out. */
export function left(r: FeedRow, now: number): string {
  if (r.paused_since !== undefined) return 'paused'
  let secs: number | undefined
  if (r.left_s !== undefined && r.left_at !== undefined) secs = r.left_s - (now - r.left_at) / 1000
  else if (r.eta_s !== undefined && r.started !== undefined) secs = r.eta_s - (now - r.started) / 1000
  if (secs === undefined) return 'no ETA'
  return secs >= 0 ? `~${dur(secs)} left` : `over by ${dur(-secs)}`
}
