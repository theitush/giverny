#!/usr/bin/env python3
"""Time how long a Claude tab takes to become usable (giverny#265).

Plays a Giverny tab without the window: a pty, the user's own `$SHELL` with
its rc files, the environment a tab gets (`TERM=xterm-256color`,
`TERM_PROGRAM=giverny`), and the replies alacritty_terminal gives to the
terminal queries Claude Code sends at boot (DA1, kitty keyboard flags, CPR,
DECRQM). Every inherited `GIVERNY_*` and `CLAUDE*` variable is dropped and
the XDG dirs point at a scratch folder, so the hooks and the status line of
the claude under test reach neither the running Giverny nor its feeds.

"Usable" is measured twice: `drawn`, the first time the input box's `❯` is
on screen, and `keys`, the moment two letters typed at `drawn` show up in
it. Nothing is ever submitted; the tab is killed once measured.

    tab_boot.py shell                         # shell spawn -> rc done
    tab_boot.py cold     [-- claude args]     # spawn shell, type claude at once
    tab_boot.py warmterm [-- claude args]     # shell already up, then type claude
    tab_boot.py warmclaude --resume-id SID    # claude already up, /resume SID in it
    tab_boot.py idle --secs 60                # RSS/CPU of an idle booted claude

Each run prints one JSON line (with /proc/loadavg before and after).
"""

import argparse
import json
import os
import pty
import re
import select
import shlex
import signal
import struct
import sys
import termios
import fcntl
import time

ROWS, COLS = 30, 120
SCRATCH = os.environ.get("TAB_BOOT_SCRATCH", "/tmp/g265")
PROMPT = "❯"  # ❯, Claude Code's input-box glyph

CSI_RE = re.compile(rb"\x1b\[[0-9;?<>=!]*[ -/]*[@-~]")
OSC_RE = re.compile(rb"\x1b\][^\x07\x1b]*(\x07|\x1b\\)")
ESC_RE = re.compile(rb"\x1b[()][0-9A-Za-z]|\x1b[=>78DEHMNOPZc\\]")


def strip(b: bytes) -> str:
    b = OSC_RE.sub(b"", b)
    b = CSI_RE.sub(b" ", b)
    b = ESC_RE.sub(b"", b)
    return b.decode("utf-8", "replace")


EXTRA_ENV = {}


def tab_env():
    env = {
        k: v
        for k, v in os.environ.items()
        if not (k.startswith("GIVERNY_") or k.startswith("CLAUDE") or k == "FORCE_HYPERLINK")
    }
    for d in ("config", "runtime", "data"):
        os.makedirs(f"{SCRATCH}/{d}", mode=0o700, exist_ok=True)
    env.update(
        TERM="xterm-256color",
        COLORTERM="truecolor",
        TERM_PROGRAM="giverny",
        XDG_CONFIG_HOME=f"{SCRATCH}/config",
        XDG_RUNTIME_DIR=f"{SCRATCH}/runtime",
        XDG_DATA_HOME=f"{SCRATCH}/data",
    )
    env.update(EXTRA_ENV)
    return env


def loadavg():
    with open("/proc/loadavg") as f:
        return " ".join(f.read().split()[:3])


class Tab:
    """A pty running the user's shell, answering terminal queries."""

    def __init__(self, cwd, argv=None):
        self.t0 = time.monotonic()
        shell = os.environ.get("SHELL") or "/bin/bash"
        argv = argv or [shell]
        env = tab_env()
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(cwd)
            os.execvpe(argv[0], argv, env)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, COLS * 9, ROWS * 18))
        self.pid, self.fd = pid, fd
        self.log = []  # (t, bytes)
        self.pending = b""

    def now(self):
        return time.monotonic() - self.t0

    def write(self, b: bytes):
        os.write(self.fd, b)

    def _answer(self, b: bytes):
        # Replies alacritty_terminal 0.26 gives (Giverny's engine); others get
        # none, as there.
        data = self.pending + b
        for m in re.finditer(rb"\x1b\[(\??)([0-9;]*)(\$?)([a-zA-Z])", data):
            priv, params, dollar, final = m.groups()
            if final == b"c" and not priv and params in (b"", b"0"):
                self.write(b"\x1b[?6c")
            elif final == b"u" and priv == b"?":
                self.write(b"\x1b[?0u")
            elif final == b"n" and params == b"6":
                self.write(b"\x1b[1;1R")
            elif final == b"p" and dollar and priv == b"?":
                self.write(b"\x1b[?" + params + b";0$y")
        self.pending = data[-16:]

    def pump(self, until, pred=None):
        """Read output until `until` (monotonic) or `pred(self)` is true."""
        while True:
            left = until - time.monotonic()
            if left <= 0:
                return False
            r, _, _ = select.select([self.fd], [], [], min(left, 0.05))
            if r:
                try:
                    b = os.read(self.fd, 1 << 16)
                except OSError:
                    return False
                if not b:
                    return False
                self.log.append((self.now(), b))
                self._answer(b)
            if pred and pred(self):
                return True

    def text_since(self, t):
        return strip(b"".join(b for (ts, b) in self.log if ts >= t))

    def first(self, needle, since=0.0):
        """Time of the first chunk at/after `since` whose cumulative text has `needle`."""
        acc = b""
        for ts, b in self.log:
            if ts < since:
                continue
            acc += b
            if needle in strip(acc):
                return ts
        return None

    def descendants(self):
        kids = {}
        for p in os.listdir("/proc"):
            if not p.isdigit():
                continue
            try:
                with open(f"/proc/{p}/stat") as f:
                    st = f.read()
                ppid = int(st[st.rindex(")") + 2 :].split()[1])
            except (OSError, ValueError):
                continue
            kids.setdefault(ppid, []).append(int(p))
        out, todo = [], [self.pid]
        while todo:
            p = todo.pop()
            out.append(p)
            todo.extend(kids.get(p, []))
        return out

    def kill(self):
        # Claude first, the way a person leaves it (^U, then ^D twice), so it
        # writes its startup profile and drops its live-session entry; then
        # whatever is left, by signal.
        if len(self.descendants()) > 1:
            self.write(b"\x15")
            self.pump(time.monotonic() + 0.3)
            for _ in range(2):
                self.write(b"\x04")
                self.pump(time.monotonic() + 0.3)
            self.pump(time.monotonic() + 6, lambda t: len(t.descendants()) <= 1)
        procs = self.descendants()
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for p in reversed(procs):
                try:
                    os.kill(p, sig)
                except ProcessLookupError:
                    pass
            time.sleep(0.3)
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.fd)


def shell_ready(tab, since, timeout=20):
    # Typed as `__RD""Y__` so the pty's echo of the typing never matches.
    tab.write(b'echo __RD""Y__\r')
    tab.pump(time.monotonic() + timeout, lambda t: "__RDY__" in t.text_since(since))
    return tab.first("__RDY__", since)


def squash(s):
    return re.sub(r"\s+", "", s)


def wait_prompt(tab, since, timeout, needle=None, box=True):
    """Time the screen is usable, then when typed keys appear in it.

    Usable is the input box's glyph drawn (`box`) and, for a resumed
    conversation, `needle` (words from its last reply, compared without
    whitespace, since the renderer wraps and spaces them) on screen too."""
    need = squash(needle) if needle else None

    def ready(t):
        text = t.text_since(since)
        return (not box or PROMPT in text) and (need is None or need in squash(text))

    if not tab.pump(time.monotonic() + timeout, ready):
        return None, None
    marks = []
    if box:
        marks.append(tab.first(PROMPT, since))
    if need:
        acc = b""
        for ts, b in tab.log:
            if ts < since:
                continue
            acc += b
            if need in squash(strip(acc)):
                marks.append(ts)
                break
    drawn = max(marks)
    tab.write(b"zq")
    sent = tab.now()
    tab.pump(time.monotonic() + 15, lambda t: "zq" in t.text_since(sent))
    keys = tab.first("zq", sent)
    return drawn, keys


def claude_cmd(args, cwd):
    words = ["command", "claude", *map(shlex.quote, args)]
    return f"cd {shlex.quote(cwd)} && {' '.join(words)}\r".encode()


def transcript_stat(sid):
    root = os.path.expanduser("~/.claude/projects")
    for d in os.listdir(root):
        p = os.path.join(root, d, sid + ".jsonl")
        if os.path.exists(p):
            st = os.stat(p)
            return {"path": p, "size": st.st_size, "mtime": st.st_mtime}
    return None


def proc_tree_cost(tab, secs):
    """RSS/PSS and CPU of the tab's processes over `secs` of idling."""
    hz = os.sysconf("SC_CLK_TCK")

    def sample():
        rows = {}
        for p in tab.descendants():
            try:
                with open(f"/proc/{p}/stat") as f:
                    st = f.read()
                name = st[st.index("(") + 1 : st.rindex(")")]
                fields = st[st.rindex(")") + 2 :].split()
                ticks = int(fields[11]) + int(fields[12])
                pss = 0
                with open(f"/proc/{p}/smaps_rollup") as f:
                    for line in f:
                        if line.startswith("Pss:"):
                            pss = int(line.split()[1])
                rows[p] = (name, ticks, pss)
            except (OSError, ValueError):
                pass
        return rows

    a = sample()
    tab.pump(time.monotonic() + secs)
    b = sample()
    procs = []
    for p, (name, ticks, pss) in b.items():
        t0 = a.get(p, (name, ticks, pss))[1]
        procs.append({"pid": p, "name": name, "pss_mb": round(pss / 1024, 1), "cpu_pct": round(100 * (ticks - t0) / hz / secs, 2)})
    return {
        "pss_mb": round(sum(x["pss_mb"] for x in procs), 1),
        "cpu_pct": round(sum(x["cpu_pct"] for x in procs), 2),
        "procs": procs,
    }


PERF_DIR = os.path.expanduser("~/.claude/startup-perf")


def startup_profile(since_wall):
    """Claude's own CLAUDE_CODE_PROFILE_STARTUP marks (ms from its process
    start) from the newest report written after `since_wall`."""
    try:
        files = [os.path.join(PERF_DIR, f) for f in os.listdir(PERF_DIR) if f.endswith(".json")]
    except OSError:
        return None
    files = [f for f in files if os.stat(f).st_mtime >= since_wall]
    if not files:
        return None
    newest = max(files, key=lambda f: os.stat(f).st_mtime)
    with open(newest) as f:
        d = json.load(f)
    return {
        "file": newest,
        "nodeBootMs": d.get("nodeBootMs"),
        "phases": d.get("metadata"),
        "marks": {m["name"]: round(m["startTime"], 1) for m in d.get("marks", [])},
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["shell", "cold", "warmterm", "warmclaude", "idle"])
    ap.add_argument("--cwd", default=os.path.expanduser("~"))
    ap.add_argument("--resume-id")
    ap.add_argument("--needle", help="words of the resumed conversation's last reply")
    ap.add_argument("--settle", type=float, default=3.0, help="warm-up idle before t0")
    ap.add_argument("--secs", type=float, default=60)
    ap.add_argument("--timeout", type=float, default=60)
    ap.add_argument("--raw", help="dump the raw pty log here")
    ap.add_argument("--label", default="")
    ap.add_argument("--env", action="append", default=[], help="K=V for the tab")
    ap.add_argument("--profile", action="store_true", help="CLAUDE_CODE_PROFILE_STARTUP=1, and record its marks")
    argv = sys.argv[1:]
    cut = argv.index("--") if "--" in argv else len(argv)
    a = ap.parse_args(argv[:cut])
    a.claude_args = argv[cut + 1 :]
    EXTRA_ENV.update(kv.split("=", 1) for kv in a.env)
    if a.profile:
        EXTRA_ENV["CLAUDE_CODE_PROFILE_STARTUP"] = "1"
    wall0 = time.time()

    rec = {"mode": a.mode, "label": a.label, "cwd": a.cwd, "args": a.claude_args, "load_before": loadavg()}
    before = transcript_stat(a.resume_id) if a.resume_id else None
    tab = Tab(a.cwd)
    try:
        if a.mode == "shell":
            rec["shell_ready"] = shell_ready(tab, 0.0)
        elif a.mode == "cold":
            args = a.claude_args + (["--resume", a.resume_id] if a.resume_id else [])
            tab.write(claude_cmd(args, a.cwd))
            rec["drawn"], rec["keys"] = wait_prompt(tab, 0.0, a.timeout, a.needle)
        elif a.mode == "warmterm":
            rec["shell_ready"] = shell_ready(tab, 0.0)
            tab.pump(time.monotonic() + a.settle)
            t0 = tab.now()
            args = a.claude_args + (["--resume", a.resume_id] if a.resume_id else [])
            tab.write(claude_cmd(args, a.cwd))
            d, k = wait_prompt(tab, t0, a.timeout, a.needle)
            rec["drawn"] = d and d - t0
            rec["keys"] = k and k - t0
        elif a.mode in ("warmclaude", "idle"):
            tab.write(claude_cmd(a.claude_args, a.cwd))
            d, k = wait_prompt(tab, 0.0, a.timeout)
            rec["boot_drawn"], rec["boot_keys"] = d, k
            tab.write(b"\x15")  # ^U: clear the two test letters
            tab.pump(time.monotonic() + a.settle)
            if a.mode == "idle":
                rec["idle"] = proc_tree_cost(tab, a.secs)
            else:
                t0 = tab.now()
                tab.write(b"/resume " + a.resume_id.encode())
                tab.pump(time.monotonic() + 1.0)
                t1 = tab.now()
                tab.write(b"\r")
                # The box is on screen throughout; the conversation's last
                # reply showing up is what says the resume is done.
                d, k = wait_prompt(tab, t1, a.timeout, a.needle, box=False)
                rec["typed_cmd"] = t1 - t0
                rec["drawn"] = d and d - t1
                rec["keys"] = k and k - t1
    finally:
        rec["load_after"] = loadavg()
        if a.raw:
            with open(a.raw, "wb") as f:
                for ts, b in tab.log:
                    f.write(b"\n@@%.3f@@" % ts + b)
        tab.kill()
    if a.profile:
        rec["profile"] = startup_profile(wall0)
    if a.resume_id:
        after = transcript_stat(a.resume_id)
        rec["transcript_size"] = before and before["size"]
        rec["transcript_touched"] = before != after
    for k, v in list(rec.items()):
        if isinstance(v, float):
            rec[k] = round(v, 3)
    print(json.dumps(rec), flush=True)


if __name__ == "__main__":
    sys.exit(main())
