#!/usr/bin/env python3
# subagentStatusLine for worker-view-spike (giverny#186): hides Claude Code's own
# subagent panel, or shows it on one worker. Claude Code runs this about every
# 5 s while workers live, with {"tasks":[{id,...}]} on stdin. A row answered with
# content "" is dropped; a row not answered is drawn natively; with every row
# dropped the whole panel (main too) is gone (giverny#3).
# The flag file (written by the mod): empty or missing = hide all; "<agentId>" =
# drop every row but that one (main stays, so one Down/Down/Enter reaches it).
import json, os, sys, time
FLAG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'state', 'strip')
LOG = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'logs', 'strip.jsonl')
try:
    payload = json.load(sys.stdin)
except Exception:
    payload = {}
try:
    keep = open(FLAG).read().strip()
except OSError:
    keep = ''
ids = [t.get('id') for t in payload.get('tasks', []) if t.get('id')]
for i in ids:
    if i != keep:
        print(json.dumps({'id': i, 'content': ''}))
with open(LOG, 'a') as f:
    f.write(json.dumps({'t': int(time.time() * 1000), 'keep': keep, 'ids': ids}) + '\n')
