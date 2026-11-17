#!/usr/bin/env python3
"""cut.py <edl> <out.tsv>: an edit list into compose.ps1's timeline (TSV: kind, epoch ms, arg).
EDL lines: `HH:MM:SS[.s] <in|out|cap|speed> [arg]`, local time on DAY; `#` starts a comment.
in = uncut, out = cut; cap "<tags>|<text>"; speed <x>. Prints the cut's length."""
import sys, datetime as dt
DAY = "2026-11-17"
ev = []
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.rstrip("\n")
    if not line.strip() or line.lstrip().startswith("#"):
        continue
    t, kind, *rest = line.split(None, 2)
    ms = int(dt.datetime.fromisoformat(f"{DAY} {t}").timestamp() * 1000)
    ev.append((ms, {"in": "uncut", "out": "cut"}.get(kind, kind), rest[0] if rest else ""))
ev.sort(key=lambda e: e[0])
first = min(m for m, k, _ in ev if k == "uncut"); last = max(m for m, k, _ in ev if k == "cut")
rows = [("start", first - 1, ""), ("cut", first - 1, "")] + [(k, m, a) for m, k, a in ev] + [("end", last + 1, "")]
with open(sys.argv[2], "w", encoding="utf-8") as f:
    for k, m, a in rows:
        f.write(f"{k}\t{m}\t{a}\n")
# length: walk the segments with their speeds
total = 0.0; on = False; speed = 1.0; prev = first
for m, k, a in ev:
    if on: total += (m - prev) / 1000 / speed
    prev = m
    if k == "uncut": on = True
    elif k == "cut": on = False
    elif k == "speed": speed = float(a)
print(f"cut length {int(total // 60)}:{int(total % 60):02d}")
