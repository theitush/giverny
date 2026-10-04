#!/usr/bin/env python3
# subagentStatusLine for strip-spike (giverny#186). Claude Code runs this while
# workers live, with {"tasks":[...]} on stdin; we answer one {"id","content"}
# line per row. The data comes from ../state/rows.json, which the mod writes
# ({rows: {agentId: {name, stage, startedMs, etaMs, tokens, status}}});
# ../state/mode picks a test variant. Every call is logged to ../logs/strip-calls.jsonl.
import json, os, sys, time
HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATE = os.path.join(HERE, 'state')
LOG = os.path.join(HERE, 'logs', os.environ.get('SS_LOG', 'strip-calls') + '.jsonl')
now = int(time.time() * 1000)
raw = sys.stdin.read()
try:
    payload = json.loads(raw)
except Exception:
    payload = {}


def rd(name, default=''):
    try:
        return open(os.path.join(STATE, name)).read().strip()
    except OSError:
        return default


mode = rd('mode', 'ours') or 'ours'
try:
    data = json.loads(rd('rows.json', '{}'))
except Exception:
    data = {}
rows = data.get('rows', {})
tasks = [t for t in payload.get('tasks', []) if isinstance(t, dict) and t.get('id')]

E = '\x1b['
R = E + '0m'
def tc(r, g, b): return f'{E}38;2;{r};{g};{b}m'
def c256(n): return f'{E}38;5;{n}m'
BOLD, DIM = E + '1m', E + '2m'
STAGE = {'reading': (95, 135, 175), 'coding': (215, 175, 0), 'building': (215, 135, 0),
         'testing': (0, 175, 135), 'landing': (0, 135, 0), 'waiting': (215, 95, 95)}


def dur(ms):
    s = max(0, int(ms // 1000))
    return f'{s // 60}m{s % 60:02d}s' if s >= 60 else f'{s}s'


def bar(frac, n=10):
    k = max(0, min(n, round(frac * n)))
    return '█' * k + '░' * (n - k)


def ours(t):
    r = rows.get(t['id'])
    if not r:
        return None  # not ours: let the native row stand
    el = now - r.get('startedMs', now)
    eta = r.get('etaMs')
    left = max(0, eta - now) if eta else None
    frac = el / max(1, el + left) if left is not None else 0
    stage = r.get('stage', 'running')
    col = tc(*STAGE.get(stage.split(':')[0], (0, 135, 215)))
    tok = t.get('tokenCount') or r.get('tokens', 0)
    tok = f'{tok / 1000:.1f}k' if tok >= 1000 else str(tok)
    return (f'{BOLD}{r.get("name", "?")}{R} {col}{stage}{R} {DIM}{dur(el)}{R}'
            f' {col}{bar(frac)}{R} ETA {dur(left) if left is not None else "?"} {DIM}{tok} tok{R}')


out = []
if mode == 'pass':
    pass  # answer nothing: every row native
elif mode == 'ansi':
    for i, t in enumerate(tasks):
        out.append({'id': t['id'], 'content': [
            f'{tc(255, 95, 135)}truecolour{R} {c256(39)}256-colour{R} {BOLD}bold{R} {DIM}dim{R} {E}4munder{R} {bar(0.4)}',
            f'{E}7mreverse{R} {E}3mitalic{R} {tc(0, 200, 0)}{BOLD}green-bold{R} ●◐✓✗ 漢字 emoji🔥',
        ][i % 2]})
elif mode == 'wide':
    for i, t in enumerate(tasks):
        out.append({'id': t['id'], 'content': f'W{i} ' + ''.join(str(k % 10) for k in range(300)) + ' END'})
elif mode == 'multiline':
    for i, t in enumerate(tasks):
        out.append({'id': t['id'], 'content': f'line-one of {i}\nline-two of {i}\nline-three of {i}'})
elif mode == 'plain':
    for i, t in enumerate(tasks):
        out.append({'id': t['id'], 'content': f'ROW{i} {t["id"][:6]}'})
else:
    for t in tasks:
        c = ours(t)
        if c is not None:
            out.append({'id': t['id'], 'content': c})
    if mode == 'reorder':
        out.reverse()
    elif mode == 'drop1' and out:
        out[0]['content'] = ''
    elif mode == 'extra':
        ids = [t['id'] for t in tasks]
        out.append({'id': 'not-a-task-id', 'content': 'EXTRA row for an id the payload did not hold'})
        for aid, r in rows.items():
            if r.get('status') == 'done' and aid not in ids:
                out.append({'id': aid, 'content': f'DONE {r.get("name")} kept?'})
for o in out:
    print(json.dumps(o))
with open(LOG, 'a') as f:
    f.write(json.dumps({'t': now, 'mode': mode, 'argv': sys.argv[1:],
                        'env': {k: os.environ.get(k) for k in ('COLUMNS', 'LINES', 'CLAUDE_PROJECT_DIR', 'TERM')},
                        'payload': payload, 'raw_len': len(raw), 'answered': len(out)}) + '\n')
