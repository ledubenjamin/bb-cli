"""Regression checks for the PTY harness, including unresponsive children."""

import importlib.util
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("terminal", Path(__file__).with_name("terminal.py"))
terminal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(terminal)


class TerminalHarnessTests(unittest.TestCase):
    def test_quiet_open_terminal_does_not_block(self):
        master, slave = pty.openpty()
        try:
            os.set_blocking(master, False)
            captured = bytearray()
            self.assertFalse(terminal.read_available(master, captured))
            terminal.drain(master, captured)
            self.assertEqual(captured, b"")
        finally:
            os.close(master)
            os.close(slave)

    def check_stalled_child(self, global_timeout):
        with tempfile.TemporaryDirectory(prefix="bb-terminal-stall-") as directory:
            binary = Path(directory) / "stalled"
            binary.write_text(f"#!{sys.executable}\n" + """
import os
import signal
import sys
import time
import tty

if sys.argv[1:3] == ["context", "create"]:
    sys.exit(0)
signal.signal(signal.SIGINT, signal.SIG_IGN)
tty.setraw(0)
os.write(2, b"[y/N] ")
while True:
    time.sleep(1)
""")
            binary.chmod(0o700)
            children = []
            original_popen = subprocess.Popen

            def spawn(*args, **kwargs):
                child = original_popen(*args, **kwargs)
                if "--child" in args[0]:
                    children.append(child)
                    if global_timeout:
                        terminal.arm_timeout(0.5)
                return child

            error = TimeoutError if global_timeout else subprocess.TimeoutExpired
            try:
                with patch.object(terminal.subprocess, "Popen", side_effect=spawn):
                    with self.assertRaises(error):
                        terminal.exercise(str(binary), True, False,
                                          exit_timeout=10 if global_timeout else 0.2)
            finally:
                terminal.arm_timeout(30)
            self.assertEqual(len(children), 1)
            self.assertIsNotNone(children[0].returncode, "stalled child was not reaped")
            self.assertLess(children[0].returncode, 0, "stalled child was not terminated")

    def test_process_timeout_cleans_up_stalled_child(self):
        self.check_stalled_child(global_timeout=False)

    def test_global_deadline_cleans_up_stalled_child(self):
        self.check_stalled_child(global_timeout=True)


if __name__ == "__main__":
    terminal.arm_timeout(30)
    unittest.main(argv=[sys.argv[0]])
