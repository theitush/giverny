import { test, expect, mock } from 'claude-code/testing'

// The pane's tree validates on both surfaces, wide and narrow, and a row
// press expands it to show the brief (giverny#186 spike).
test('agents pane draws and expands on terminal and desktop', async ($, on) => {
  const clock = mock.clock(on, { now: 1_790_000_000_000 })
  on('session.start', () => ({ cwd: '/tmp' }))
  on('command.register', () => ({}) as never)
  on('ui.open', () => ({ isPlaced: true }) as never)
  on('ui.log', () => undefined as never)
  await $.session.start({ cwd: '/tmp', surface: 'terminal', isInteractive: true } as never)
  await clock.advance(3000)
  for (const surface of ['terminal', 'desktop'] as const) {
    for (const bodyColumns of [130, 60]) {
      const ui = await $.ui.mount({
        plugin: 'agents-pane-spike', surface, component: 'Pane', requestId: 'agents',
        props: { title: 'Agents', isFocused: false, bodyColumns, placement: 'dock', scroll: { top: 0, height: 40, of: 40 } as never, view: {} as never },
      })
      expect(await ui.find({ key: 'row:giverny#183' })).toBeDefined()
      expect(await ui.find({ text: /no ETA/ })).toBeDefined()
      await ui.press({ key: 'row:giverny#35' })
      expect(await ui.find({ text: /Review: ita/ })).toBeDefined()
      expect(await ui.find({ key: 'brief:giverny#35' })).toBeDefined()
      await ui.press({ key: 'row:giverny#35' })
      await ui.unmount()
    }
  }
})
