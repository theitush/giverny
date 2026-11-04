import type { ClientModule } from 'claude-code'

// The panel's scroll bar, for a band set shorter than its rows: Claude Code gives the band its
// wheel only over a tree taller than the whole site, so this is the mouse's way along such a window.

export type BarProps = { rows: number; thumb: number; at: number; ink: string; lit: string }
/** A post: the pointer's row in the bar, which runs `rows` rows. */
export type BarPost = { y: number; rows: number }
type Held = { over: boolean; held: boolean }

const Bar: ClientModule<BarProps, Held> = (props, surface) => {
  const { Box, Text } = surface.elements
  const now = surface.state ?? { over: false, held: false }
  const set = (change: Partial<Held>) => surface.setState({ ...(surface.state ?? now), ...change })
  surface.onPointer(e => {
    if (e.type === 'enter') set({ over: true })
    else if (e.type === 'leave') set({ over: false })
    else if (e.type === 'down') {
      set({ held: true })
      surface.post({ y: e.y, rows: props.rows })
    } else if (e.type === 'move' && surface.state?.held) surface.post({ y: e.y, rows: props.rows })
    else if (e.type === 'up') set({ held: false })
  })
  const ink = now.over || now.held ? props.lit : props.ink
  const cells = Array.from({ length: props.rows }, (_, i) => (i >= props.at && i < props.at + props.thumb ? '█' : '│'))
  return (
    <Box flexDirection="column">
      {cells.map(c => (
        <Text color={ink}>{c}</Text>
      ))}
    </Box>
  )
}

export default Bar
