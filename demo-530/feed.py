#!/usr/bin/env python3
# feed.py <step>: the manager tab's feed, by hand, as docs/management-panel.md says a writer writes it.
#   plan | run12 | land1 | run3 | land2 | land3
import json, os, sys, time
R = "/home/ita/giverny/.claude/review/530/rig"
SID = open(f"{R}/ids.txt").read().split()[0]
D = f"{R}/live/xdg/config/giverny/feeds"
STATE = f"{R}/live/feed-state.json"
now = int(time.time() * 1000)
def brief(n): return f"{R}/briefs/demo{n}.md"
if sys.argv[1] == "plan":
    rows = [
        {"key": "demo#1", "stage": "planned", "title": "Changelog for v1.8", "eta_s": 120, "brief": brief(1)},
        {"key": "demo#2", "stage": "planned", "title": "Release notes", "eta_s": 30, "brief": brief(2)},
        {"key": "demo#3", "stage": "planned", "title": "Upgrade notes", "eta_s": 60, "after": "demo#1", "brief": brief(3)},
        {"key": "demo#4", "stage": "planned", "title": "Publish v1.8", "eta_s": 600, "held": "waiting on y0av's review", "brief": brief(4)},
    ]
else:
    rows = json.load(open(STATE))
by = {r["key"]: r for r in rows}
def run(k): by[k].update(stage="running", started=now)
def land(k, landing, review=None):
    by[k].update(stage="done", ended=now, landing=landing)
    if review: by[k]["review"] = review
step = sys.argv[1]
if step == "run12": run("demo#1"); run("demo#2")
elif step == "land1": land("demo#1", "Review — y0av", "y0av — the changelog's wording — CHANGELOG.md")
elif step == "run3": run("demo#3")
elif step == "land2": land("demo#2", "Done")
elif step == "land3": land("demo#3", "Done")
json.dump(rows, open(STATE, "w"))
done = sum(r["stage"] == "done" for r in rows)
feed = {"version": 1, "session": SID, "rows": rows,
        "footer": {"text": f"feed written by hand from docs/management-panel.md · {done} of {len(rows)} done"}}
os.makedirs(f"{D}/managers/roots", exist_ok=True)
json.dump({"session": SID}, open(f"{D}/managers/{SID}.json", "w"))
tmp = f"{D}/{SID}.json.tmp"
json.dump(feed, open(tmp, "w"), indent=1)
os.replace(tmp, f"{D}/{SID}.json")
print(step, [(r["key"], r["stage"]) for r in rows])
