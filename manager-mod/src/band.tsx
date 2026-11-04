import type { EngineInterface, RenderInput } from 'claude-code'

import type { DisplayRow, Section } from '../types'

import { readHidden, readPage, readSaid, readView } from './state'
import { drawBox, goBack, openRow, viewedWords } from './worker'

// Draws the display rows above the prompt, as the #521 prototype did. A running row is the
// way to its worker: there is no separate line of buttons.

const NEXT_SHOWN = 3
const DONE_SHOWN = 3
const LABEL: Record<Section, string> = { running: 'RUNNING', next: 'NEXT UP', done: 'DONE' }
const COLOR: Record<Section, 'green' | 'yellow' | undefined> = { running: 'green', next: 'yellow', done: undefined }

const cut = (s: string, n: number) => (s.length <= n ? s : `${s.slice(0, Math.max(0, n - 1))}…`)

/** One table line: the task, its times, and what is going on, all cut to `width`. */
export function line(r: DisplayRow, keyW: number, width: number): string {
  const times = `${r.elapsed.padStart(5)}  ${r.left.padEnd(12)}`
  const room = width - keyW - times.length - 13
  // Wide enough: a NOW column, as the Giverny panel has. Narrow: the title takes it all.
  const whatW = room >= 60 ? Math.floor(room * 0.4) : 0
  const titleW = Math.max(0, room - whatW)
  const cells = [cut(r.key, keyW).padEnd(keyW), cut(r.title, titleW).padEnd(titleW), times]
  if (whatW > 0) cells.push(cut(r.what, whatW))
  return cells.join('  ').trimEnd()
}

/** The band's tree, or undefined with nothing to show. */
export async function drawBand($: EngineInterface, e: RenderInput<'AbovePrompt'>) {
  const model = await readView($)
  const words = await readSaid($)
  const page = await readPage($)
  const box = await drawBox($, e)
  if (e.props.hasSurvey || (await readHidden($)) || model.rows.length === 0) return undefined
  const { Box, Text, Button } = $.ui.resolve(e)
  const width = Math.max(40, e.props.bodyColumns)
  const keyW = Math.min(12, Math.max(4, ...model.rows.map(r => r.key.length)))

  const group = (section: Section, shown: number) =>
    model.rows
      .filter(r => r.section === section)
      .slice(0, shown)
      .map((r, i) => {
        const dim = section === 'done'
        const parts = [
          <Text color={COLOR[section]} bold={!dim && i === 0} dimColor={dim}>{(i === 0 ? LABEL[section] : '').padEnd(8)}</Text>,
          <Text color={r.over ? 'red' : undefined} dimColor={dim}>{` ${line(r, keyW, width)}`}</Text>,
        ]
        return (
          <Button key={`row-${r.id}`} plain onPress={() => openRow($, r, e.props.view, e.requestId)}>
            {parts}
          </Button>
        )
      })

  return (
    <Box flexDirection="column">
      <Box flexDirection="row" gap={2}>
        <Text bold>Manager</Text>
        <Text dimColor>{`${model.running} running · ${model.next} next · ${model.done} done`}</Text>
        {(e.props.view?.agentId || page) && (
          <Button key="back" hotkey="0" plain onPress={() => goBack($, e.props.view)}>
            back to the manager
          </Button>
        )}
        {words && <Text dimColor>{cut(words, 50)}</Text>}
      </Box>
      {box}
      {e.props.view?.agentId && <Text dimColor wrap="truncate">{viewedWords(model, e.props.view)}</Text>}
      {group('running', model.running)}
      {group('next', NEXT_SHOWN)}
      {model.next > NEXT_SHOWN && <Text dimColor>{`         +${model.next - NEXT_SHOWN} more next up`}</Text>}
      {group('done', DONE_SHOWN)}
    </Box>
  )
}
