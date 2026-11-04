import type { ClientModule } from 'claude-code'

// A row's hover text, a tooltip with egui's fill and edge. It sits in a Box the row's hover group
// reveals, which has no delay; this waits the rest out. It takes its size from what it draws, which
// is one cell across and no rows until it shows: laid out at no size while hidden, called again a
// cell wide when revealed, and over no row until then. So no timer runs but over the row under the
// pointer. A tooltip takes no pointer: the pointer coming onto it puts it away, and what is under
// it then has the pointer, as egui's is passed through.

/** egui's `tooltip_delay` in the version Giverny pins (egui 0.35.0, style.rs): the panel's wait. */
const WAIT_MS = 500

export type TipProps = { lines: string[]; ink: string; fill: string; edge: string | null }
type Waiting = { shown: boolean; stop?: () => void; away?: boolean }

const Tip: ClientModule<TipProps, Waiting> = (props, surface) => {
  const { Box, Text } = surface.elements
  const now = surface.state
  const revealed = surface.columns > 0
  surface.onPointer(e => {
    if ((e.type === 'enter' || e.type === 'move') && surface.state?.shown) surface.setState({ shown: false, away: true })
  })
  if (revealed && !now?.stop && !now?.shown && !now?.away) {
    const stop = surface.every(WAIT_MS, () => {
      stop()
      surface.setState({ shown: true })
    })
    surface.setState({ shown: false, stop })
  } else if (!revealed && (now?.stop || now?.shown || now?.away)) {
    now.stop?.()
    surface.setState({ shown: false })
  }
  if (!revealed || !now?.shown) return <Box width={1} height={0} />
  return (
    <Box flexDirection="column" {...(props.edge === null ? {} : { borderStyle: 'round', borderColor: props.edge })} backgroundColor={props.fill}>
      {props.lines.map(l => (
        <Text color={props.ink} backgroundColor={props.fill}>{` ${l} `}</Text>
      ))}
    </Box>
  )
}

export default Tip
