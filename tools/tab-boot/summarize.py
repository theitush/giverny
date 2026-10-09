#!/usr/bin/env python3
"""Medians of a tab_boot.py / matrix.sh run log, per label.

    summarize.py runs.jsonl [--heavy 10]

A run whose 1-minute load average (before or after) is at or over `--heavy`
is counted apart and flagged; medians are given over all runs and over the
lighter ones.
"""

import argparse
import json
import statistics as st
from collections import defaultdict

# Spans of Claude Code's own startup profile, ms (marks named in its
# CLAUDE_CODE_PROFILE_STARTUP report).
SPANS = [
    ("runtime+imports", None, "main_tsx_entry"),
    ("init", "main_tsx_entry", "action_handler_start"),
    ("uds-inbox", "setup_uds_start", "setup_uds_end"),
    ("tools", "action_after_setup", "action_tools_loaded"),
    ("commands+skills", "action_tools_loaded", "action_commands_loaded"),
    ("telemetry", "action_commands_loaded", "telemetry_init_start"),
    ("mcp-configs", "telemetry_init_start", "action_mcp_configs_loaded"),
    ("plugins+hooks", "action_mcp_configs_loaded", "action_after_hooks"),
    ("canary", "action_after_hooks", "fullscreen_canary_armed"),
    ("to-canary", None, "fullscreen_canary_armed"),
]


def med(xs):
    xs = [x for x in xs if x is not None]
    return (round(st.median(xs), 3), len(xs)) if xs else (None, 0)


def load1(r):
    return max(float(r["load_before"].split()[0]), float(r["load_after"].split()[0]))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log")
    ap.add_argument("--heavy", type=float, default=10.0)
    a = ap.parse_args()
    by = defaultdict(list)
    for line in open(a.log):
        line = line.strip()
        if line.startswith("{"):
            r = json.loads(line)
            by[r.get("label") or r["mode"]].append(r)

    keys = ["shell_ready", "drawn", "keys", "boot_drawn", "boot_keys"]
    print(f"{'label':28} {'n':>2} {'heavy':>5} {'load1 med':>9}  " + "  ".join(f"{k:>14}" for k in keys))
    for label, rs in by.items():
        heavy = [r for r in rs if load1(r) >= a.heavy]
        light = [r for r in rs if load1(r) < a.heavy]
        cells = []
        for k in keys:
            m, n = med(r.get(k) for r in rs)
            ml, _ = med(r.get(k) for r in light)
            cells.append(f"{'' if m is None else m:>7}/{'' if ml is None else ml:>6}")
        lm = st.median(load1(r) for r in rs)
        print(f"{label:28} {len(rs):>2} {len(heavy):>5} {lm:>9.2f}  " + "  ".join(cells))
    print("(cells: median over all runs / over runs under the heavy-load mark)")

    print("\nClaude Code startup profile, median ms per span")
    print(f"{'label':28} " + " ".join(f"{s[0][:13]:>13}" for s in SPANS))
    for label, rs in by.items():
        prof = [r["profile"]["marks"] for r in rs if r.get("profile")]
        if not prof:
            continue
        cells = []
        for _, a0, b0 in SPANS:
            vals = [m[b0] - (m[a0] if a0 else 0) for m in prof if b0 in m and (a0 is None or a0 in m)]
            v, _ = med(vals)
            cells.append(f"{'' if v is None else round(v):>13}")
        print(f"{label:28} " + " ".join(cells))

    touched = [r["label"] for rs in by.values() for r in rs if r.get("transcript_touched")]
    if touched:
        print(f"\nresumes that appended Claude Code's own metadata to the transcript: {len(touched)}")


if __name__ == "__main__":
    main()
