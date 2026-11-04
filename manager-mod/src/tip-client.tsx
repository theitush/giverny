import type { ClientModule } from 'claude-code'

// A row's hover text. It sits in a Box the row's hover group reveals, which has no delay; this
// waits the rest out: laid out at no size while hidden, called again at its real size when
// revealed and at none when hidden. So no timer runs but over the row under the pointer.

/** egui's `tooltip_delay` in the version Giverny pins (egui 0.35.0, style.rs): the panel's wait. */
const WAIT_MS = 500

export type TipProps = { lines: string[]; ink: string; fill: string }
type Waiting = { shown: boolean; stop?: () => void }

const Tip: ClientModule<TipProps, Waiting> = (props, surface) => {
  const { Box, Text } = surface.elements
  const now = surface.state
  const revealed = surface.columns > 0
  if (revealed && !now?.stop && !now?.shown) {
    const stop = surface.every(WAIT_MS, () => {
      stop()
      surface.setState({ shown: true })
    })
    surface.setState({ shown: false, stop })
  } else if (!revealed && (now?.stop || now?.shown)) {
    now.stop?.()
    surface.setState({ shown: false })
  }
  if (!revealed || !now?.shown) return <Box />
  return (
    <Box flexDirection="column">
      {props.lines.map(l => (
        <Text color={props.ink} backgroundColor={props.fill}>{` ${l} `}</Text>
      ))}
    </Box>
  )
}

export default Tip
