import type { ClientModule } from 'claude-code'

// A working worker's last line on its page, what a spinner says in its view: what it is at, and for
// how long. It ticks on the drawing thread, so a long page's tree is not drawn again every second.
// The page draws it anew, with a new key, each time the worker's transcript moves.

const TICK_MS = 1000
const GLYPHS = ['✶', '✻', '✽', '✢', '·', '✢', '✽', '✻']

export type DoingProps = { words: string; since: number; ink: string }
type Ticks = { from: number; ticks: number }

const clock = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`

const Doing: ClientModule<DoingProps, Ticks> = (props, surface) => {
  const { Text } = surface.elements
  const now = surface.state
  if (!now) {
    surface.every(TICK_MS, () => {
      const was = surface.state
      if (was) surface.setState({ ...was, ticks: was.ticks + 1 })
    })
    surface.setState({ from: props.since, ticks: 0 })
  }
  const ticks = now?.ticks ?? 0
  return <Text color={props.ink} wrap="truncate">{`${GLYPHS[ticks % GLYPHS.length]} ${props.words}…  ${clock((now?.from ?? props.since) + ticks)}`}</Text>
}

export default Doing
