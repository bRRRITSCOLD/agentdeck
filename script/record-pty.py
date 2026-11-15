#!/usr/bin/env python3
"""Records an agent CLI's raw terminal output as state-rule fixtures (spec §7).

Runs argv in a PTY of a fixed size, follows a steps file that types into it, and cuts the
raw output into one `<name>.bytes` file per `mark`, each holding the bytes since the one
before, so feeding them in order rebuilds the screen. It also writes `marks.log`: each
mark's byte count, the foreground process name, and the last five OSC 0/2 title, OSC 9;4
and OSC 7501 sequences of that segment.

    script/record-pty.py --out DIR --steps FILE [--cols 120] [--rows 40] -- argv...

Steps, one per line (`#` starts a comment):

    wait <regex> [timeout_s]   until the regex matches the output since the last step, raw or
                               with escape sequences stripped (default timeout 60 s; fails after)
    sleep <seconds>
    send <text>                types ASCII text; Python escapes such as \\r and \\x1b work. Sleep
                               before a \\r: a TUI takes text and Enter typed at once for a paste
    mark <name>                writes the bytes since the last mark to <name>.bytes
    end                        stops: SIGHUP to the process group, then waits for exit

It answers the terminal queries a TUI waits on at startup (cursor position, device
attributes, keyboard flags, colours), as the daemon's terminal does. The output is not
scrubbed: check it for home paths, account names and session ids before committing.
"""

import argparse
import fcntl
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

ESCAPES = re.compile(rb"\x1b\[[0-9;?<=>!]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78DEHMNOZc]")
OSC = re.compile(rb"\x1b\](0|2|9;4|7501);([^\x07\x1b]*)(?:\x07|\x1b\\)")
QUERIES = [
    (b"\x1b[6n", lambda r, c: b"\x1b[%d;%dR" % (r, c)),
    (b"\x1b[c", lambda r, c: b"\x1b[?62;22c"),
    (b"\x1b[>c", lambda r, c: b"\x1b[>0;0;0c"),
    (b"\x1b[?u", lambda r, c: b"\x1b[?0u"),
    (b"\x1b]10;?\x1b\\", lambda r, c: b"\x1b]10;rgb:d8d8/d8d8/d8d8\x1b\\"),
    (b"\x1b]10;?\x07", lambda r, c: b"\x1b]10;rgb:d8d8/d8d8/d8d8\x07"),
    (b"\x1b]11;?\x1b\\", lambda r, c: b"\x1b]11;rgb:1818/1818/1818\x1b\\"),
    (b"\x1b]11;?\x07", lambda r, c: b"\x1b]11;rgb:1818/1818/1818\x07"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", required=True)
    parser.add_argument("--steps", required=True)
    parser.add_argument("--cols", type=int, default=120)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("argv", nargs="+")
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)

    env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(args.argv[0], args.argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.cols, 0, 0))
    os.kill(pid, signal.SIGWINCH)

    rec = Recorder(fd, pid, args)
    log = open(os.path.join(args.out, "marks.log"), "w")
    try:
        for line in open(args.steps):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            verb, _, rest = line.partition(" ")
            if verb == "wait":
                pattern, _, timeout = rest.rpartition(" ")
                if not timeout.replace(".", "").isdigit():
                    pattern, timeout = rest, "60"
                rec.wait(pattern, float(timeout))
            elif verb == "sleep":
                rec.pump(float(rest))
            elif verb == "send":
                os.write(fd, rest.encode().decode("unicode_escape").encode("latin-1"))
                rec.since_step = b""
            elif verb == "mark":
                rec.mark(rest, log)
            elif verb == "end":
                break
            else:
                sys.exit(f"unknown step: {line}")
    finally:
        rec.end(log)


class Recorder:
    def __init__(self, fd, pid, args):
        self.fd, self.pid, self.args = fd, pid, args
        self.segment = b""
        self.since_step = b""
        self.alive = True

    def pump(self, seconds):
        deadline = time.monotonic() + seconds
        while self.alive and time.monotonic() < deadline:
            ready, _, _ = select.select([self.fd], [], [], max(0, min(0.05, deadline - time.monotonic())))
            if not ready:
                continue
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                self.alive = False
                return
            self.segment += chunk
            self.since_step += chunk
            for query, reply in QUERIES:
                for _ in range(chunk.count(query)):
                    os.write(self.fd, reply(self.args.rows, self.args.cols))

    def wait(self, pattern, timeout):
        regex = re.compile(pattern.encode())
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if regex.search(self.since_step) or regex.search(ESCAPES.sub(b"", self.since_step)):
                self.since_step = b""
                return
            if not self.alive:
                self.fail(f"exited while waiting for {pattern!r}")
            self.pump(0.1)
        self.fail(f"timed out waiting for {pattern!r}")

    def fail(self, why):
        with open(os.path.join(self.args.out, "failed.bytes"), "wb") as out:
            out.write(self.segment)
        sys.exit(f"{why}; the bytes since the last mark are in failed.bytes")

    def mark(self, name, log):
        with open(os.path.join(self.args.out, f"{name}.bytes"), "wb") as out:
            out.write(self.segment)
        osc = [f"{kind.decode()};{text.decode(errors='replace')}" for kind, text in OSC.findall(self.segment)]
        log.write(f"{name}: {len(self.segment)} bytes, process={self.foreground()!r}, osc={osc[-5:]!r}\n")
        log.flush()
        self.segment = b""

    def foreground(self):
        try:
            with open(f"/proc/{os.tcgetpgrp(self.fd)}/comm") as comm:
                return comm.read().strip()
        except OSError:
            return None

    def end(self, log):
        if self.alive:
            try:
                os.killpg(self.pid, signal.SIGHUP)
            except ProcessLookupError:
                pass
        _, status = os.waitpid(self.pid, 0)
        log.write(f"exit: status={status}\n")


if __name__ == "__main__":
    main()
