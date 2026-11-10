"""Exercise the real terminal modes; called by the Unix Cargo integration test."""

import errno
import fcntl
import os
import pty
import select
import signal
import subprocess
import sys
import tempfile
import termios
import time

WAIT_TIMEOUT = 5


def timeout(_signal, _frame):
    raise TimeoutError("terminal harness exceeded its deadline")


def arm_timeout(seconds):
    signal.signal(signal.SIGALRM, timeout)
    signal.setitimer(signal.ITIMER_REAL, seconds)


def read_available(master, captured):
    try:
        chunk = os.read(master, 4096)
    except BlockingIOError:
        return False
    except OSError as error:
        if error.errno == errno.EIO:
            return False
        raise
    captured.extend(chunk)
    return bool(chunk)


def drain(master, captured):
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline:
        if not select.select([master], [], [], 0.05)[0]:
            return
        # A PTY can report readiness without readable bytes after a hangup.
        if not read_available(master, captured):
            return
    raise TimeoutError("terminal output did not stop after process exit")


def wait_until(predicate, child, description):
    deadline = time.monotonic() + WAIT_TIMEOUT
    while time.monotonic() < deadline:
        if predicate():
            return
        if child.poll() is not None:
            raise AssertionError(f"process exited before {description}")
        time.sleep(0.01)
    raise AssertionError(f"timeout waiting for {description}")


def exercise(binary, external_signal, password, exit_timeout=WAIT_TIMEOUT):
    scenario = f"{'password' if password else 'confirmation'}/{'SIGINT' if external_signal else 'Ctrl-C'}"

    def progress(stage):
        print(f"{scenario}: {stage}", file=sys.stderr, flush=True)

    with tempfile.TemporaryDirectory(prefix="bb-terminal-") as config:
        env = {k: v for k, v in os.environ.items()
               if not k.startswith("BB_") and k != "BITBUCKET_REPO_FULL_NAME"}
        env.update(BB_CONFIG_DIR=config, TERM="xterm-256color")
        if not password:
            progress("creating context")
            subprocess.run([binary, "context", "create", "remove-me"],
                           env=env, check=True, capture_output=True, timeout=WAIT_TIMEOUT)
        master, slave = pty.openpty()
        child = None
        captured = bytearray()
        try:
            os.set_blocking(master, False)
            original = termios.tcgetattr(slave)
            args = ["auth", "login", "--profile", "audit"] if password else ["context", "delete", "remove-me"]
            progress("starting prompt")
            # Set up the controlling terminal after exec, so Popen cannot hang
            # inside a preexec_fn before returning the child handle to cleanup.
            child = subprocess.Popen([sys.executable, "-I", __file__, "--child", binary, *args],
                                     env=env, stdin=slave, stdout=subprocess.PIPE,
                                     stderr=slave, start_new_session=True)

            def prompt_ready():
                read_available(master, captured)
                marker = b"Bitbucket token: " if password else b"[y/N] "
                return marker in captured and not (termios.tcgetattr(slave)[3] & termios.ICANON)

            progress("waiting for raw terminal prompt")
            wait_until(prompt_ready, child, "raw terminal prompt")
            if password:
                assert not (termios.tcgetattr(slave)[3] & termios.ECHO), "token input is echoed"
                os.write(master, b"sensitive-terminal-token")
                # Let the terminal consume the partial input before an external signal.
                time.sleep(0.1)
            progress("interrupting prompt")
            if external_signal:
                os.kill(child.pid, signal.SIGINT)
            else:
                os.write(master, b"\x03")
            progress("waiting for process exit")
            stdout, _ = child.communicate(timeout=exit_timeout)
            assert child.returncode == 2, child.returncode
            assert stdout == b"", stdout
            assert termios.tcgetattr(slave) == original, "terminal settings were not restored"
            progress("draining terminal output")
            drain(master, captured)
            assert b"sensitive-terminal-token" not in captured, captured
            assert b"cancelled" in captured or b"interrupted" in captured, captured
            if not password:
                progress("checking context was preserved")
                listed = subprocess.run([binary, "context", "list", "--json", "name"],
                                        env=env, check=True, capture_output=True, timeout=WAIT_TIMEOUT)
                assert b"remove-me" in listed.stdout, listed.stdout
            progress("passed")
        finally:
            try:
                if child is not None:
                    if child.poll() is None:
                        try:
                            os.killpg(child.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                    child.wait(timeout=WAIT_TIMEOUT)
                    child.stdout.close()
            finally:
                os.close(master)
                os.close(slave)


if __name__ == "__main__":
    if sys.argv[1] == "--child":
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        os.execv(sys.argv[2], sys.argv[2:])
    else:
        # Leave time for cleanup before the Rust integration test's 45 s limit.
        arm_timeout(40)
        for external in (False, True):
            for secret in (False, True):
                exercise(sys.argv[1], external, secret)
