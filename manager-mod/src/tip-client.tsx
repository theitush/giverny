import type { ClientModule } from 'claude-code'
const Tip: ClientModule<{ lines: string[] }, { at: number; n: number }> = (props, s) => {
  const { Text, Box } = s.elements
  const st = s.state
  if (!st) s.setState({ at: Date.now(), n: 1 })
  return Box({ flexDirection: 'column', children: [Text({ children: `SPIKE cols=${s.columns} rows=${s.rows} n=${st?.n ?? 0} age=${st ? Date.now() - st.at : -1}` })] })
}
export default Tip
