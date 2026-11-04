import type { ClientModule } from 'claude-code'

// The rule along the band's top, which the person drags, as the top edge of Giverny's panel. It
// says where the pointer is against the height it was drawn at, so a post made before the last
// height reached the screen lands on the same target.

export type GripProps = { width: number; rows: number; ink: string; lit: string }
/** A post: `y` is the pointer's row against the rule's, `rows` the height drawn with it. */
export type GripPost = { y: number; rows: number; done: boolean }
type Held = { over: boolean; held: boolean; moved: boolean }

const Grip: ClientModule<GripProps, Held> = (props, surface) => {
  const { Text } = surface.elements
  const now = surface.state ?? { over: false, held: false, moved: false }
  const set = (change: Partial<Held>) => surface.setState({ ...(surface.state ?? now), ...change })
  surface.onPointer(e => {
    const held = surface.state?.held ?? false
    if (e.type === 'enter') set({ over: true })
    else if (e.type === 'leave') set({ over: false })
    else if (e.type === 'down') set({ held: true, moved: false })
    else if (e.type === 'move' && held && (e.y !== 0 || surface.state?.moved)) {
      if (!surface.state?.moved) set({ moved: true })
      surface.post({ y: e.y, rows: props.rows, done: false })
    } else if (e.type === 'up' && held) {
      if (surface.state?.moved) surface.post({ y: e.y, rows: props.rows, done: true })
      set({ held: false, moved: false })
    }
  })
  return <Text color={now.over || now.held ? props.lit : props.ink}>{'─'.repeat(props.width)}</Text>
}

export default Grip
