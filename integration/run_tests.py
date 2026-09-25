#!/usr/bin/env python3
"""Drive the real login flow over WSS. Stdlib only."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import socket
import ssl
import struct
import subprocess
import sys
import time
import uuid


ORIGIN = "https://127.0.0.1:8443"
HOST = "127.0.0.1"
PORT = 8443
USER = "testuser"
PASSWORD = "testpass"


class WsError(Exception):
    pass


class WsClient:
    def __init__(self, origin: str = ORIGIN) -> None:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection((HOST, PORT), timeout=10)
        self.sock = ctx.wrap_socket(raw, server_hostname=HOST)
        self.sock.settimeout(15)
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        req = (
            "GET /console HTTP/1.1\r\n"
            f"Host: {HOST}:{PORT}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            f"Origin: {origin}\r\n"
            "\r\n"
        )
        self.sock.sendall(req.encode("ascii"))
        header = b""
        while b"\r\n\r\n" not in header:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise WsError("no handshake response")
            header += chunk
        status = header.split(b"\r\n", 1)[0]
        if b"101" not in status:
            raise WsError(f"upgrade rejected: {status!r}\n{header[:400]!r}")
        self.buf = header.split(b"\r\n\r\n", 1)[1]

    def send(self, opcode: int, payload: bytes) -> None:
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        header = bytes([0x80 | opcode])
        n = len(payload)
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack("!H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack("!Q", n)
        self.sock.sendall(header + mask + masked)

    def send_text(self, text: str) -> None:
        self.send(0x1, text.encode("utf-8"))

    def send_bin(self, data: bytes) -> None:
        self.send(0x2, data)

    def send_resize(self, cols: int, rows: int) -> None:
        self.send_text(json.dumps({"type": "resize", "cols": cols, "rows": rows}))

    def _recv_exact(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise WsError("socket closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self, timeout: float = 10.0) -> tuple[int, bytes]:
        self.sock.settimeout(timeout)
        b0, b1 = self._recv_exact(2)
        opcode = b0 & 0x0F
        masked = b1 & 0x80
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", self._recv_exact(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", self._recv_exact(8))[0]
        mask = self._recv_exact(4) if masked else b""
        payload = self._recv_exact(n)
        if masked:
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        if opcode == 0x8:
            return opcode, payload
        if opcode == 0x9:
            self.send(0xA, payload)
            return self.recv(timeout)
        if opcode == 0xA:
            return self.recv(timeout)
        return opcode, payload

    def close(self) -> None:
        try:
            self.send(0x8, b"")
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


class Term:
    def __init__(self, ws: WsClient) -> None:
        self.ws = ws
        self.buf = bytearray()

    def wait_contains(self, needle: str, timeout: float = 15.0) -> str:
        deadline = time.time() + timeout
        raw = needle.encode("utf-8", "replace")
        while time.time() < deadline:
            if raw in self.buf:
                return self.buf.decode("utf-8", "replace")
            try:
                op, data = self.ws.recv(timeout=max(0.1, deadline - time.time()))
            except (TimeoutError, socket.timeout, WsError):
                continue
            if op == 0x8:
                break
            if op in (0x1, 0x2):
                self.buf.extend(data)
        text = self.buf.decode("utf-8", "replace")
        raise AssertionError(f"timeout waiting for {needle!r} in {text[-400:]!r}")

    def send_line(self, line: str) -> None:
        self.ws.send_bin((line + "\r").encode("utf-8"))

    def send_raw(self, data: bytes) -> None:
        self.ws.send_bin(data)


def pids_for_user(user: str) -> list[str]:
    cmds = [
        ["ps", "-u", user, "-o", "pid=", "-o", "cmd="],
        ["sudo", "-n", "ps", "-u", user, "-o", "pid=", "-o", "cmd="],
    ]
    out = ""
    for cmd in cmds:
        try:
            proc = subprocess.run(cmd, text=True, capture_output=True, check=False)
        except FileNotFoundError:
            continue
        if proc.returncode == 0 or proc.stdout.strip():
            out = proc.stdout
            break
    lines = []
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        if "consoled" in line:
            continue
        lines.append(line)
    return lines


def wait_shell(term: Term, timeout: float = 30.0) -> None:
    """Wait until a user shell is running. Output must not appear in the typed line.

    The probe is re-sent until answered: login/PAM restores the terminal with
    tcsetattr(TCSAFLUSH) after reading the password, which discards any input
    that reached the PTY before that point. Under load the first probe can
    land in that window and be silently dropped (a real terminal behaves the
    same), so a single send is racy.
    """
    deadline = time.time() + timeout
    while True:
        term.send_line("expr 200 + 23")
        try:
            term.wait_contains("223", timeout=min(5.0, max(0.1, deadline - time.time())))
            return
        except AssertionError:
            if time.time() >= deadline:
                raise


def login(term: Term, password: str, expect_shell: bool = True) -> None:
    term.wait_contains("login:")
    term.send_line(USER)
    term.wait_contains("Password:")
    term.send_line(password)
    if expect_shell:
        wait_shell(term)


def test_bad_password() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, "wrong-password", expect_shell=False)
    term.wait_contains("Login incorrect")
    ws.close()
    print("ok: bad password rejected")


def test_success_and_tty() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, PASSWORD)
    term.send_line("tty")
    text = term.wait_contains("/dev/pts/")
    assert "/dev/pts/" in text
    term.send_line("python3 -c \"import os; print(os.isatty(0), os.isatty(1), os.isatty(2))\"")
    term.wait_contains("True True True")
    print("ok: login, tty, isatty")
    ws.close()


def test_resize() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, PASSWORD)
    # Do not resize while bash may be (re)drawing a prompt. Each time readline
    # prepares the terminal it does TIOCGWINSZ followed by TIOCSWINSZ with the
    # value it just read (readline rltty.c set_winsize). A resize that lands
    # between those two calls is overwritten with the old size, and `stty size`
    # then prints the stale "24 80". Sending the resize right after the
    # previous command's output hits exactly that window.
    #
    # Instead park bash inside a command line: once GATE is printed readline
    # is done with this line and will not touch the window size again until
    # the command finishes. `read` (no -e, so no readline) waits for a line.
    # consoled applies frames in order, so TIOCSWINSZ is done before the
    # newline that releases `read` is written, and `stty` runs after both.
    term.send_line("echo GATE-$((6 * 7)); read -r _; echo SZ-$(stty size)-END")
    term.wait_contains("GATE-42")
    ws.send_resize(100, 30)
    term.send_line("")
    term.wait_contains("SZ-30 100-END")
    print("ok: stty size follows resize")
    ws.close()


def test_ctrl_c() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, PASSWORD)
    marker = f"C-{uuid.uuid4().hex[:8]}"
    term.send_line("sleep 1000")
    time.sleep(0.4)
    term.send_raw(b"\x03")
    term.send_line(f"echo done-{marker}")
    term.wait_contains(f"done-{marker}")
    print("ok: Ctrl+C interrupted sleep")
    ws.close()


def test_ctrl_z() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, PASSWORD)
    term.send_line("sleep 1000")
    time.sleep(0.4)
    term.send_raw(b"\x1a")
    text = term.wait_contains("Stopped")
    assert "sleep" in text or "Stopped" in text
    term.send_line("kill %1 || true")
    term.send_line("wait 2>/dev/null || true")
    print("ok: Ctrl+Z job control")
    ws.close()


def test_cleanup() -> None:
    ws = WsClient()
    ws.send_resize(80, 24)
    term = Term(ws)
    login(term, PASSWORD)
    before = pids_for_user(USER)
    assert before, "expected a user shell after login"
    ws.close()
    deadline = time.time() + 8
    while time.time() < deadline:
        left = pids_for_user(USER)
        if not left:
            print("ok: no leftover login/shell after disconnect")
            return
        time.sleep(0.2)
    raise AssertionError(f"leftover processes: {pids_for_user(USER)}")


def test_origin_rejected() -> None:
    try:
        WsClient(origin="https://evil.example")
    except WsError as e:
        if b"403" in str(e).encode("utf-8", "replace") or "403" in str(e):
            print("ok: bad Origin rejected")
            return
        raise
    raise AssertionError("evil origin was accepted")


def main() -> int:
    test_origin_rejected()
    test_bad_password()
    test_success_and_tty()
    test_resize()
    test_ctrl_c()
    test_ctrl_z()
    test_cleanup()
    return 0


if __name__ == "__main__":
    sys.exit(main())
