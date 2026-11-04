import type { Shell } from '../types'

import { fmtSpan } from './time'

// What a background shell is called on a row: crates/claude/src/bg_shells.rs gist and named.
// `cd ~/poly; T=…; npx wrangler deploy > $T/d.log 2>&1; echo $?` is `wrangler deploy`.

const SETUP = new Set(['cd', 'pushd', 'popd', 'export', 'set', 'unset', 'source', '.', 'echo', 'printf', 'true', ':', 'mkdir', 'local', 'declare', 'trap', 'ulimit', 'umask', 'chmod', 'ln'])
const INTERPRETERS = new Set(['node', 'python', 'python3', 'bash', 'sh', 'zsh', 'deno', 'bun', 'ruby', 'perl', 'tsx', 'ts-node', 'Rscript'])
const WRAPPERS = new Set(['npx', 'sudo', 'nohup', 'time', 'env', 'exec', 'command', 'nice', 'stdbuf', 'setsid', 'timeout', 'ionice', 'chrt'])
const MAX = 32

const base = (word: string) => {
  const w = word.replace(/\/+$/, '')
  return w.split('/').pop() || w
}
const isAssignment = (w: string) => /^[A-Za-z_][A-Za-z0-9_]*=/.test(w)
const isRedirect = (w: string) => /^(>|<|2>|&>|1>)/.test(w)

/** A command line cut into its simple commands, as words. Rough, as a name needs no more. */
export function segments(cmd: string): string[][] {
  const out: string[][] = []
  let seg: string[] = []
  let word = ''
  let inWord = false
  let heredocs: string[] = []
  const endWord = () => {
    if (inWord) seg.push(word)
    word = ''
    inWord = false
  }
  let i = 0
  while (i < cmd.length) {
    const c = cmd[i++] as string
    if (c === '<' && cmd[i] === '<') {
      i++
      endWord()
      if (cmd[i] === '<') {
        i++
        seg.push('<<<')
        continue
      }
      seg.push('<<')
      if (cmd[i] === '-') i++
      while (cmd[i] === ' ' || cmd[i] === '\t') i++
      let delim = ''
      while (i < cmd.length && !/[\s;&|<>]/.test(cmd[i] as string)) {
        if (!/['"\\]/.test(cmd[i] as string)) delim += cmd[i]
        i++
      }
      if (delim) heredocs.push(delim)
    } else if (c === "'") {
      inWord = true
      while (i < cmd.length && cmd[i] !== "'") word += cmd[i++]
      i++
    } else if (c === '"') {
      inWord = true
      while (i < cmd.length && cmd[i] !== '"') {
        if (cmd[i] === '\\') i++
        if (i < cmd.length) word += cmd[i++]
      }
      i++
    } else if (c === '\\') {
      const n = cmd[i++]
      if (n !== undefined && n !== '\n') {
        inWord = true
        word += n
      }
    } else if (c === ';' || c === '|' || c === '\n') {
      endWord()
      if (cmd[i] === c) i++
      out.push(seg)
      seg = []
      if (c === '\n') {
        for (const delim of heredocs) {
          let line = ''
          while (i < cmd.length) {
            const d = cmd[i++]
            if (d !== '\n') {
              line += d
              continue
            }
            if (line.trim() === delim) break
            line = ''
          }
        }
        heredocs = []
      }
    } else if (c === '&') {
      if (inWord && word.endsWith('>')) {
        word += c
      } else if (cmd[i] === '>') {
        endWord()
        inWord = true
        word += c
      } else {
        endWord()
        if (cmd[i] === '&') i++
        out.push(seg)
        seg = []
      }
    } else if (/\s/.test(c)) {
      endWord()
    } else {
      inWord = true
      word += c
    }
  }
  endWord()
  out.push(seg)
  return out.filter(s => s.length > 0)
}

function pick(seg: string[]): string[] | undefined {
  const w = [...seg]
  for (;;) {
    while (w[0] !== undefined && isAssignment(w[0])) w.shift()
    const first = w[0]
    if (first === undefined) return undefined
    const name = base(first)
    if (WRAPPERS.has(name)) {
      w.shift()
      while (w[0] !== undefined && (w[0].startsWith('-') || !Number.isNaN(Number(w[0])))) w.shift()
      continue
    }
    const dashes = w.indexOf('--')
    if ((name.startsWith('giverny') || name === 'systemd-run') && dashes !== -1) {
      w.splice(0, dashes + 1)
      continue
    }
    return w
  }
}

const isSetup = (w: string[]) => {
  const first = w[0]
  if (first === undefined) return false
  const name = base(first)
  return SETUP.has(name) || ((name === 'cat' || name === 'tee') && w.includes('<<'))
}

/** A shell's command as a row names it: the program and its first words, past the setup. */
export function gist(cmd: string): string {
  const chosen = segments(cmd)
    .map(pick)
    .filter((w): w is string[] => w !== undefined)
  const words = chosen.find(w => !isSetup(w)) ?? chosen[0]
  if (!words) return cmd.trim()
  const interpreter = words[0] !== undefined && INTERPRETERS.has(base(words[0]))
  const kept: string[] = []
  for (const [i, w] of words.entries()) {
    const isPath = w.includes('/') && !interpreter
    if (i > 0 && (w.startsWith('-') || isRedirect(w) || isPath || kept.length >= 3)) break
    kept.push(base(w))
  }
  const one = kept.join(' ')
  return [...one].length <= MAX ? one : `${[...one].slice(0, MAX - 1).join('').trimEnd()}…`
}

const ran = (startedMs: number, now: number) => {
  const s = Math.max(0, Math.floor((now - startedMs) / 1000))
  return s < 60 ? `${s}s` : fmtSpan(s)
}

/** The shells an asleep worker waits on: the oldest's gist and how long it has run, `+N` for the rest. */
export function named(shells: Shell[], now: number): string {
  const sorted = [...shells].sort((a, b) => a.startedMs - b.startedMs)
  const first = sorted[0]
  if (!first) return ''
  const more = sorted.length === 1 ? '' : ` +${sorted.length - 1}`
  return `${gist(first.cmd)} (${ran(first.startedMs, now)})${more}`
}
