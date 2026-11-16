#!/usr/bin/env python3
# agents.py [wait <key> <running|finished|exists> [timeout s]]: the manager session's workers, from Claude Code's files.
import glob, json, os, sys, time
R = "/home/ita/giverny/.claude/review/530/rig"
SID = open(f"{R}/ids.txt").read().split()[0]
D = os.path.expanduser(f"~/.claude/projects/-home-ita-giverny--claude-acme/{SID}/subagents")
def rows():
    out = []
    for m in sorted(glob.glob(f"{D}/agent-*.meta.json"), key=os.path.getmtime):
        try: d = json.load(open(m))
        except Exception: continue
        out.append((d.get("description", ""), bool(d.get("finished")), os.path.basename(m)[6:-10]))
    return out
if len(sys.argv) > 1 and sys.argv[1] == "wait":
    key, want = sys.argv[2], sys.argv[3]
    end = time.time() + float(sys.argv[4] if len(sys.argv) > 4 else 300)
    while time.time() < end:
        for desc, fin, _ in rows():
            if key in desc.split() and (want == "exists" or (want == "finished") == fin):
                print(f"{key} {want}"); sys.exit(0)
        time.sleep(0.5)
    print(f"timeout waiting for {key} {want}"); sys.exit(1)
for r in rows(): print(*r)
