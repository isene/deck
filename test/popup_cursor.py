#!/usr/bin/env python3
"""Run deck on a pseudo terminal, open the key popup (C-?), and check that
the cursor is hidden while the popup is up and shown again after it closes.

    cargo build --release && test/popup_cursor.py

Claude is a stand-in that reads prompts and never answers, and HOME is a
temporary folder, so nothing touches a real session.
"""

import fcntl
import os
import pty
import select
import struct
import sys
import tempfile
import termios
import time

HIDE, SHOW = b"\x1b[?25l", b"\x1b[?25h"


def read(fd: int, seconds: float) -> bytes:
    out, end = b"", time.time() + seconds
    while time.time() < end:
        if select.select([fd], [], [], 0.1)[0]:
            try:
                out += os.read(fd, 65536)
            except OSError:
                break
    return out


def cursor(b: bytes) -> str:
    h, s = b.rfind(HIDE), b.rfind(SHOW)
    return "hidden" if h > s else "shown" if s > h else "untouched"


def main() -> int:
    deck = os.path.join(os.path.dirname(__file__), "..", "target", "release", "deck")
    tmp = tempfile.mkdtemp()
    claude = os.path.join(tmp, "claude")
    with open(claude, "w") as fh:
        fh.write("#!/bin/sh\ncat >/dev/null\n")  # the shell keeps stdout open
    os.chmod(claude, 0o755)
    pid, fd = pty.fork()
    if pid == 0:
        env = dict(os.environ, HOME=tmp, DECK_CLAUDE=claude, TERM="xterm-256color")
        os.execve(deck, [deck], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    read(fd, 2.0)
    os.write(fd, b"\x1b[I")  # focus in
    before = read(fd, 1.0)
    os.write(fd, b"\x1b[63;5u")  # C-?
    during = read(fd, 1.5)
    os.write(fd, b"\x1b")
    after = read(fd, 1.5)
    os.kill(pid, 9)
    got = (cursor(before), b"this help" in during, cursor(during), cursor(after))
    want = ("shown", True, "hidden", "shown")
    print("cursor before, popup drawn, cursor during, cursor after:", got)
    return 0 if got == want else 1


if __name__ == "__main__":
    sys.exit(main())
