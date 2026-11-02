#!/usr/bin/env python3
"""Send identical SMTP conversations to both servers and diff the reply codes."""
import os
import signal
import socket
import subprocess
import sys
import time

RUST = os.path.join(os.path.dirname(__file__), "..", "target", "release", "rsmtp-sink")
POSTFIX_PORT, RUST_PORT = 22525, 22526

CONVERSATIONS = {
    "full transaction": ["EHLO c", "MAIL FROM:<a@b>", "RCPT TO:<c@d>", "DATA", "hello\r\n.", "QUIT"],
    "helo": ["HELO c", "QUIT"],
    "two messages": ["EHLO c", "MAIL FROM:<a>", "RCPT TO:<b>", "DATA", "x\r\n.",
                     "MAIL FROM:<a>", "RCPT TO:<b>", "DATA", "y\r\n.", "QUIT"],
    "two recipients": ["EHLO c", "MAIL FROM:<a>", "RCPT TO:<b>", "RCPT TO:<c>", "DATA", "x\r\n.", "QUIT"],
    "lowercase": ["ehlo c", "mail from:<a>", "rcpt to:<b>", "data", "x\r\n.", "quit"],
    "rset and noop": ["EHLO c", "MAIL FROM:<a>", "RSET", "NOOP", "QUIT"],
    "vrfy": ["EHLO c", "VRFY someone", "QUIT"],
    "unknown command": ["EHLO c", "BOGUS", "QUIT"],
    "mail before greeting": ["MAIL FROM:<a>", "QUIT"],
    "rcpt before mail": ["EHLO c", "RCPT TO:<a>", "QUIT"],
    "data before rcpt": ["EHLO c", "MAIL FROM:<a>", "DATA", "QUIT"],
    "mail with no argument": ["EHLO c", "MAIL", "QUIT"],
}


def read_reply(f):
    """Return the code of one SMTP reply, consuming continuation lines."""
    while True:
        line = f.readline()
        if not line:
            return "EOF"
        if line[3:4] != b"-":
            return line[:3].decode()


def talk(port, script):
    with socket.create_connection(("127.0.0.1", port), timeout=3) as s:
        f = s.makefile("rb")
        codes = [read_reply(f)]
        for command in script:
            s.sendall(command.encode() + b"\r\n")
            codes.append(read_reply(f))
        return codes


def main():
    procs = [
        subprocess.Popen(["/usr/libexec/postfix/smtp-sink", f"127.0.0.1:{POSTFIX_PORT}", "64"]),
        subprocess.Popen([RUST, f"127.0.0.1:{RUST_PORT}", "64"], stderr=subprocess.DEVNULL),
    ]
    time.sleep(0.5)
    differences = 0
    try:
        for name, script in CONVERSATIONS.items():
            postfix, rust = talk(POSTFIX_PORT, script), talk(RUST_PORT, script)
            same = postfix == rust
            differences += not same
            print(f"{'same' if same else 'DIFF'}  {name}")
            if not same:
                print(f"      postfix {postfix}\n      rust    {rust}")
    finally:
        for proc in procs:
            proc.send_signal(signal.SIGINT)
            proc.wait()
    print(f"\n{len(CONVERSATIONS) - differences}/{len(CONVERSATIONS)} conversations match")
    return 0


if __name__ == "__main__":
    sys.exit(main())
