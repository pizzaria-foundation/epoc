#!/usr/bin/env python3
"""Tests for the host half of the bridge.

Run with `python3 tools/test_epocadb.py` or `python3 -m unittest discover tools`.

The end-to-end cases speak the device's side of the protocol over real loopback
sockets. That is deliberate: the bug that made `logcat` print nothing was a
framing mismatch between two files that each looked correct on its own, and
only a test that puts real bytes on a real socket can see it.
"""

import importlib.machinery
import importlib.util
import socket
import tempfile
import threading
import time
import unittest
from pathlib import Path

# The CLI has no .py extension, so the loader has to be named explicitly.
_loader = importlib.machinery.SourceFileLoader("epocadb", str(Path(__file__).with_name("epocadb")))
epocadb = importlib.util.module_from_spec(importlib.util.spec_from_loader("epocadb", _loader))
_loader.exec_module(epocadb)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ── framing ───────────────────────────────────────────────────────

class LineReaderTests(unittest.TestCase):
    def socketpair_reader(self) -> tuple[socket.socket, epocadb.LineReader]:
        """A connected pair; the reader reads what is written to the other end."""
        a, b = socket.socketpair()
        self.addCleanup(a.close)
        self.addCleanup(b.close)
        return a, epocadb.LineReader(b)

    def test_lf_only_lines_are_read(self):
        # The device's log channel separates lines with a bare LF: the ring buffer
        # appends one byte. A reader that waits for CRLF never returns a single
        # line, which is exactly why logcat printed nothing at all.
        peer, reader = self.socketpair_reader()
        peer.sendall(b"connecting to DC2\nauth key negotiated\n")
        reader.pump()
        self.assertEqual(reader.take_line(), "connecting to DC2")
        self.assertEqual(reader.take_line(), "auth key negotiated")
        self.assertIsNone(reader.take_line())

    def test_crlf_lines_are_read_without_the_cr(self):
        peer, reader = self.socketpair_reader()
        peer.sendall(b"OK pong\r\n")
        reader.pump()
        self.assertEqual(reader.take_line(), "OK pong")

    def test_a_line_split_across_reads_is_not_lost(self):
        # The old reader dropped its partial buffer on every timeout, so a line
        # straddling a poll boundary lost its front half and desynchronised
        # everything after it.
        peer, reader = self.socketpair_reader()
        peer.sendall(b"OK PUSH C:\\Data")
        reader.pump()
        self.assertIsNone(reader.take_line())
        peer.sendall(b"\\f.bin 5\r\n")
        reader.pump()
        self.assertEqual(reader.take_line(), "OK PUSH C:\\Data\\f.bin 5")

    def test_payload_sharing_a_segment_with_its_header_is_readable(self):
        # The host-side twin of the device bug: the header and its bytes arrive
        # together, so the payload must come out of the same buffer the header
        # did rather than from a fresh read on the socket.
        peer, reader = self.socketpair_reader()
        peer.sendall(b"DATA 5\r\nhello")
        reader.pump()
        self.assertEqual(reader.take_line(), "DATA 5")
        self.assertEqual(reader.take_exactly(5), b"hello")

    def test_take_exactly_waits_for_every_byte(self):
        peer, reader = self.socketpair_reader()
        peer.sendall(b"abc")
        reader.pump()
        self.assertIsNone(reader.take_exactly(5), "must not hand back a short read")
        peer.sendall(b"de")
        reader.pump()
        self.assertEqual(reader.take_exactly(5), b"abcde")

    def test_binary_payload_survives_the_reader(self):
        peer, reader = self.socketpair_reader()
        payload = bytes(range(256)) * 4
        peer.sendall(b"DATA %d\r\n" % len(payload) + payload)
        reader.pump()
        self.assertEqual(reader.take_line(), f"DATA {len(payload)}")
        self.assertEqual(reader.take_exactly(len(payload)), payload)

    def test_a_closed_peer_raises(self):
        peer, reader = self.socketpair_reader()
        peer.close()
        with self.assertRaises(epocadb.PeerClosed):
            reader.pump()


# ── log rendering ─────────────────────────────────────────────────

class RenderTests(unittest.TestCase):
    # stdout is not a tty under the test runner, so no ANSI is added — which lets us
    # assert the parsing without matching escape codes.
    def test_untagged_line_is_indented_plain(self):
        self.assertEqual(epocadb.render_log("hello world"), "  hello world")

    def test_tagged_line_is_recognised_but_plain_off_tty(self):
        self.assertEqual(epocadb.render_log("[app] focus uid=0x1"), "  [app] focus uid=0x1")

    def test_log_subtag_does_not_break_parsing(self):
        # [log:telegram] → tag "log"; must parse without error.
        self.assertEqual(epocadb.render_log("[log:telegram] hi"), "  [log:telegram] hi")

    def test_a_bare_bracket_is_not_mistaken_for_a_tag(self):
        self.assertEqual(epocadb.render_log("[incomplete"), "  [incomplete")

    def test_an_unknown_tag_still_renders(self):
        # A tag is an app's own category, not a closed set: one the CLI has never heard of
        # prints plain rather than being dropped or mangled.
        self.assertEqual(
            epocadb.render_log("[whatever] something happened"),
            "  [whatever] something happened",
        )

    def test_the_sdks_own_tags_have_colours(self):
        # The categories the SDK itself emits must be colourable, or a busy stream is
        # unscannable. An app's own tags are its business.
        for tag in ("log", "net", "ui", "mem", "gfx", "step", "recv"):
            self.assertIn(tag, epocadb._TAG_COLOR)


# ── end to end, against a fake device ─────────────────────────────

class FakeDevice:
    """The device's side of the protocol, enough to exercise the host."""

    def __init__(self, cmd_port: int, log_port: int):
        self.cmd = socket.create_connection(("127.0.0.1", cmd_port), timeout=5)
        self.log = socket.create_connection(("127.0.0.1", log_port), timeout=5)
        self.cmd.settimeout(5)
        self.reader = epocadb.LineReader(self.cmd)
        self.cmd.setblocking(True)
        self.received: bytes | None = None
        self.received_path: str | None = None

    def close(self):
        self.cmd.close()
        self.log.close()

    def _line(self, timeout=5.0) -> str:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = self.reader.take_line()
            if line is not None:
                return line
            self.cmd.setblocking(False)
            try:
                self.reader.pump()
            finally:
                self.cmd.setblocking(True)
            time.sleep(0.01)
        raise AssertionError("no line from the host in time")

    def _exact(self, n: int, timeout=5.0) -> bytes:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            data = self.reader.take_exactly(n)
            if data is not None:
                return data
            self.cmd.setblocking(False)
            try:
                self.reader.pump()
            finally:
                self.cmd.setblocking(True)
            time.sleep(0.01)
        raise AssertionError(f"only {self.reader.buffered()} of {n} payload bytes arrived")

    def ping(self) -> str:
        """One poll cycle. Returns the host's reply line."""
        self.cmd.sendall(b"REQ PING\r\n")
        return self._line()

    def log_line(self, text: str) -> None:
        """Log lines are LF-separated, as the ring buffer produces them."""
        self.log.sendall(text.encode() + b"\n")

    def poll_until_command(self, tries=200) -> str:
        for _ in range(tries):
            reply = self.ping()
            if reply != "OK pong":
                return reply
            time.sleep(0.01)
        raise AssertionError("the host never issued a command")

    def accept_push(self) -> None:
        """Carry out the push half: read DATA and its payload, then acknowledge."""
        header = self._line()
        assert header.startswith("DATA "), f"expected DATA, got {header!r}"
        self.received = self._exact(int(header[5:]))
        self.cmd.sendall(b"OK wrote\r\n")

    def serve_pull(self, body: bytes) -> None:
        self.cmd.sendall(f"OK {len(body)}\r\n".encode())
        self.cmd.sendall(f"DATA {len(body)}\r\n".encode() + body)


class ServeHarness:
    def __init__(self):
        self.cmd_port = free_port()
        self.log_port = free_port()
        self.control_port = free_port()
        self._saved = (epocadb.CMD_PORT, epocadb.LOG_PORT, epocadb.CONTROL_PORT)
        epocadb.CMD_PORT = self.cmd_port
        epocadb.LOG_PORT = self.log_port
        epocadb.CONTROL_PORT = self.control_port
        self.thread = threading.Thread(target=epocadb.cmd_serve, args=(None,), daemon=True)
        self.thread.start()
        self.device = self._connect()

    def _connect(self) -> FakeDevice:
        for _ in range(200):
            try:
                return FakeDevice(self.cmd_port, self.log_port)
            except OSError:
                time.sleep(0.02)
        raise AssertionError("serve never started listening")

    def control(self, line: str) -> None:
        for _ in range(200):
            try:
                with socket.create_connection(("127.0.0.1", self.control_port), timeout=2) as s:
                    s.sendall((line + "\r\n").encode())
                    s.recv(64)
                return
            except OSError:
                time.sleep(0.02)
        raise AssertionError("the control channel never came up")

    def stop(self):
        self.device.close()
        self.thread.join(timeout=5)
        epocadb.CMD_PORT, epocadb.LOG_PORT, epocadb.CONTROL_PORT = self._saved


class ServeTests(unittest.TestCase):
    def setUp(self):
        self.h = ServeHarness()
        self.addCleanup(self.h.stop)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def test_an_idle_host_answers_pong(self):
        self.assertEqual(self.h.device.ping(), "OK pong")

    def test_push_delivers_the_file_to_the_device(self):
        payload = bytes(range(256)) * 40  # 10 KB, well past one segment
        src = Path(self.tmp.name) / "payload.bin"
        src.write_bytes(payload)

        self.h.control(f"PUSH {src}\tC:\\Data\\payload.bin")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, f"OK PUSH C:\\Data\\payload.bin {len(payload)}")

        self.h.device.accept_push()
        self.assertEqual(self.h.device.received, payload)

    def test_push_handles_a_remote_path_containing_spaces(self):
        src = Path(self.tmp.name) / "s.bin"
        src.write_bytes(b"xyz")
        self.h.control(f"PUSH {src}\tC:\\Data\\my file.bin")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, "OK PUSH C:\\Data\\my file.bin 3")

    def test_pull_writes_to_the_local_path_that_was_asked_for(self):
        # The old CLI accepted a local path and ignored it, deriving the name from
        # the remote basename and dropping the file in the working directory.
        dest = Path(self.tmp.name) / "nested" / "report-copy.txt"
        body = b"the report body, at some length" * 100

        self.h.control(f"PULL C:\\Data\\report.txt\t{dest}")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, "OK PULL C:\\Data\\report.txt")

        self.h.device.serve_pull(body)
        for _ in range(200):
            if dest.exists() and dest.read_bytes() == body:
                break
            time.sleep(0.02)
        self.assertTrue(dest.exists(), f"{dest} was never written")
        self.assertEqual(dest.read_bytes(), body)

    def test_install_targets_a_remote_path_not_the_local_one(self):
        # The old CLI sent the host's own path as the device's destination.
        sis = Path(self.tmp.name) / "telegram.sis"
        sis.write_bytes(b"MZ-not-really-a-sis")
        self.h.control(f"INSTALL {sis}\t{epocadb.DEFAULT_INSTALL_DIR}\\telegram.sis")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, "OK INSTALL C:\\Data\\telegram.sis 19")
        self.h.device.accept_push()
        self.assertEqual(self.h.device.received, b"MZ-not-really-a-sis")

    def test_a_second_command_runs_after_the_first_completes(self):
        a = Path(self.tmp.name) / "a.bin"
        a.write_bytes(b"first")
        b = Path(self.tmp.name) / "b.bin"
        b.write_bytes(b"second")

        self.h.control(f"PUSH {a}\tC:\\Data\\a.bin")
        self.assertTrue(self.h.device.poll_until_command().startswith("OK PUSH"))
        self.h.device.accept_push()
        self.assertEqual(self.h.device.received, b"first")

        self.h.control(f"PUSH {b}\tC:\\Data\\b.bin")
        self.assertTrue(self.h.device.poll_until_command().startswith("OK PUSH"))
        self.h.device.accept_push()
        self.assertEqual(self.h.device.received, b"second")

    def test_monitor_control_forwards_verbatim_and_returns_to_idle(self):
        # `epocadb monitor enable` reaches the device as `OK CTL monitor enable`; the device
        # answers on the log channel, so serve must not wait — the next ping is a pong.
        self.h.control("CTL monitor enable")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, "OK CTL monitor enable")
        self.assertEqual(self.h.device.ping(), "OK pong")

    def test_gov_control_forwards_verbatim_and_returns_to_idle(self):
        # `epocadb gov enable` reaches the device as `OK CTL gov enable`, on the same generic
        # CTL passthrough — the bridge does not interpret it; the app on top does.
        self.h.control("CTL gov enable")
        reply = self.h.device.poll_until_command()
        self.assertEqual(reply, "OK CTL gov enable")
        self.assertEqual(self.h.device.ping(), "OK pong")

    def test_a_bad_queued_command_does_not_kill_the_server(self):
        self.h.control("NONSENSE not a real command")
        # The session must still answer, and still accept real work afterwards.
        for _ in range(50):
            if self.h.device.ping() == "OK pong":
                break
            time.sleep(0.01)
        src = Path(self.tmp.name) / "after.bin"
        src.write_bytes(b"still working")
        self.h.control(f"PUSH {src}\tC:\\Data\\after.bin")
        self.assertTrue(self.h.device.poll_until_command().startswith("OK PUSH"))

    def test_a_push_of_a_missing_local_file_does_not_kill_the_server(self):
        self.h.control("PUSH /nonexistent/file.bin\tC:\\Data\\x.bin")
        for _ in range(50):
            if self.h.device.ping() == "OK pong":
                return
            time.sleep(0.01)
        self.fail("the server stopped answering after a missing-file command")


if __name__ == "__main__":
    unittest.main(verbosity=2)
