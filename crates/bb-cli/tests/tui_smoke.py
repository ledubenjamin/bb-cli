"""End-to-end terminal checks using only the Python standard library and fake API data."""

import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from urllib.parse import parse_qs, unquote, urlencode, urlsplit
from terminal_screen import Screen


binary = str(Path(sys.argv[1]).resolve())
fixtures = json.loads(Path(__file__).with_name("fixtures").joinpath("pipelines.json").read_text())
runs = {run["uuid"]: run for run in fixtures["runs"]}
log = ("build output\n" * 6500 + "\x1b[31mCOMPILATION_ERROR échec compilation\x1b[0m\n").encode()
requests = []
search_requests = []
screens = {}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        parsed = urlsplit(self.path)
        path = unquote(parsed.path)
        params = parse_qs(parsed.query)
        query = params.get("q", [""])[0]
        match = re.search(r'name ~ "([^"\\]*)"', query)
        search = match.group(1).lower() if match else ""
        if search:
            search_requests.append((path, search))
        requests.append(path)
        status = 200
        headers = {}
        if path.endswith("/user/workspaces"):
            value = {"values": [{"workspace": {"uuid": "workspace", "slug": "acme", "name": "Example"}}]}
        elif path.endswith("/workspaces/acme/projects"):
            projects = [{"uuid": "project", "key": "APP", "name": "Applications"}]
            value = {"values": [project for project in projects if search in (project["key"] + project["name"]).lower()]}
        elif path == "/2.0/repositories/acme":
            repository = {"uuid": "repo", "slug": "service", "full_name": "acme/service", "name": "Service", "project": {"key": "APP", "name": "Applications"}}
            if search == "slow":
                time.sleep(0.8)
                repository.update(uuid="obsolete", slug="obsolete", full_name="acme/obsolete", name="Obsolete")
                value = {"values": [repository]}
            elif params.get("page") == ["2"]:
                repository.update(uuid="extra", slug="service-extra", full_name="acme/service-extra", name="Service extra")
                value = {"values": [repository]}
            else:
                value = {"values": [repository] if search in "acme/service applications app" else []}
                if search == "service":
                    value["next"] = f"http://127.0.0.1:{self.server.server_port}/2.0/repositories/acme?" + urlencode({"q": query, "page": "2"})
        elif path.endswith("/pipelines"):
            if "PARENT_STEP" in parsed.query:
                value = {"values": [run for run in runs.values() if run["trigger"]["name"] == "PARENT_STEP"]}
            else:
                # Input and redraws must remain responsive while this read is pending.
                time.sleep(1.5)
                value = {"values": [run for run in runs.values() if run["build_number"] in (177, 180)]}
        elif "/pipelines/" in path:
            parts = path.split("/pipelines/", 1)[1].split("/")
            run = parts[0]
            if len(parts) == 1:
                value = runs.get(run, {})
            elif len(parts) == 2:
                value = {"values": fixtures["steps"].get(run, [])}
            elif len(parts) == 3:
                value = next((old["step"] for old in fixtures["earlier"] if old["parent"] == run and old["step"]["uuid"] == parts[2]), {})
            else:
                body = log
                span = self.headers.get("Range", "bytes=0-").removeprefix("bytes=")
                left, right = span.split("-", 1)
                start = max(0, len(body) - int(right)) if not left else int(left)
                end = len(body) - 1 if not left or not right else min(int(right), len(body) - 1)
                status = 416 if start >= len(body) else 206
                headers["Content-Range"] = f"bytes */{len(body)}" if status == 416 else f"bytes {start}-{end}/{len(body)}"
                body = b"" if status == 416 else body[start:end + 1]
                self.reply(status, body, headers)
                return
        else:
            status = 404
            value = {"error": {"message": "Unknown fixture endpoint"}}
        self.reply(status, json.dumps(value).encode(), headers)

    def reply(self, status, body, headers):
        try:
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()


def wait_text(master, process, text, timeout=8):
    result = bytearray()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result and text in screens[master].text():
            return bytes(result)
        if select.select([master], [], [], 0.05)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            result.extend(chunk)
            rows, columns, _, _ = struct.unpack("HHHH", fcntl.ioctl(master, termios.TIOCGWINSZ, b"\0" * 8))
            screen = screens[master]
            if (screen.rows, screen.columns) != (rows, columns):
                screen.resize(rows, columns)
            screen.feed(chunk)
        if process.poll() is not None:
            break
    raise AssertionError(f"Missing terminal text: {text!r}; exit={process.poll()}; screen=\n{screens[master].text()}")


def launch(env, args):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    screens[master] = Screen(40, 140)
    original = termios.tcgetattr(slave)
    process = subprocess.Popen([binary, "tui", *args], stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True, preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
    return master, slave, original, process


def wait_until(predicate, timeout=3):
    deadline = time.monotonic() + timeout
    while not predicate():
        assert time.monotonic() < deadline, "Timed out waiting for background work"
        time.sleep(0.02)


def finish(master, slave, original, process, code):
    assert process.wait(timeout=3) == code
    assert termios.tcgetattr(slave) == original, "Terminal attributes were not restored"
    os.close(master)
    os.close(slave)
    screens.pop(master, None)


try:
    with tempfile.TemporaryDirectory(prefix="bb-tui-test-") as directory:
        env = {key: value for key, value in os.environ.items() if not key.startswith("BB_") and key not in ("BITBUCKET_REPO_FULL_NAME", "GIT_DIR", "GIT_WORK_TREE")}
        env.update(BB_CONFIG_DIR=directory, BB_TOKEN="fixture-token", BB_API_URL=f"http://127.0.0.1:{server.server_port}/2.0/", TERM="xterm-256color")
        master, slave, original, process = launch(env, ["-R", "acme/service"])
        try:
            wait_text(master, process, "Pipelines")
            start = time.monotonic()
            os.write(master, b"?")
            wait_text(master, process, "Keyboard help", timeout=1)
            response_ms = round((time.monotonic() - start) * 1000)
            os.write(master, b"\x1b")
            wait_text(master, process, "#177")
            os.write(master, b"F\r")
            wait_text(master, process, "COMPILATION_ERROR")
            assert "Explore ›" in screens[master].text()
            assert "Pipeline inspector" in screens[master].text()
            # The overlay keeps the browser context while builds preview in place.
            os.write(master, b"\x1b[D")
            wait_text(master, process, "› Builds")
            os.write(master, b"FF")
            wait_text(master, process, "acme/service · #180")
            wait_text(master, process, "Logs · #180")
            wait_until(lambda: any("/pipelines/{00000000-0000-4000-8000-000000000180}/steps/" in path and path.endswith("/log") for path in requests))
            os.write(master, b"\x1b[B")
            wait_text(master, process, "acme/service · #177")
            assert "› Builds" in screens[master].text(), "Preview stole keyboard focus"
            os.write(master, b"\x1b[C")
            wait_text(master, process, "› Steps")
            os.write(master, b"\x1b[C")
            wait_text(master, process, "› Logs")
            os.write(master, b"\x1b[D")
            wait_text(master, process, "› Steps")
            os.write(master, b"\x1b")
            wait_text(master, process, "› Pipelines")
            assert "Pipeline inspector" not in screens[master].text()
            os.write(master, b"\x1b[D")
            wait_text(master, process, "› Repositories")
            os.write(master, b"\x1b[D")
            wait_text(master, process, "› Projects")
            os.write(master, b"\x1b[C\x1b[C\r")
            wait_text(master, process, "COMPILATION_ERROR")
            os.write(master, b"/COMPILATION_ERROR\r")
            wait_text(master, process, "1 matches")
            os.write(master, b"\r")
            wait_text(master, process, "COMPILATION_ERROR")
            # Resize while inspecting a cached search hit.
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            process.send_signal(signal.SIGWINCH)
            wait_text(master, process, "Logs")
            os.write(master, b"w")
            wait_text(master, process, "Filter or type")
            os.write(master, b"acme\r")
            wait_text(master, process, "Set as default")
            os.write(master, b"\r")
            wait_text(master, process, "Default workspace saved")
            os.write(master, b"\x10")
            wait_text(master, process, "Quick open")
            os.write(master, b"slow")
            wait_until(lambda: ("/2.0/repositories/acme", "slow") in search_requests)
            os.write(master, b"\x15service")
            wait_text(master, process, "Service")
            wait_until(lambda: ("/2.0/repositories/acme", "service") in search_requests)
            # The new search must remain selected after the old HTTP response arrives.
            time.sleep(0.9)
            os.write(master, b"\x1b[6~")
            wait_text(master, process, "Service extra")
            # An OS lock delays the save while navigation stays responsive.
            with Path(directory, "config.lock").open("a+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                os.write(master, b"*")
                wait_text(master, process, "Saving favorite")
                os.write(master, b"\r")
                wait_text(master, process, "Pipelines", timeout=1)
                fcntl.flock(lock, fcntl.LOCK_UN)
            wait_text(master, process, "Added to favorites")
            os.write(master, b"\x10")
            wait_text(master, process, "Quick open")
            # Opening a project through Quick open restores the repository pane.
            os.write(master, b"\tapp")
            wait_text(master, process, "Applications")
            os.write(master, b"\r")
            wait_text(master, process, "Repositories")
            os.write(master, b"L")
            wait_text(master, process, "Search logs")
            os.write(master, b"\x1b")
            wait_text(master, process, "Repositories")
            os.write(master, b"q")
            finish(master, slave, original, process, 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
        result = subprocess.run([binary, "logs", "search", "COMPILATION_ERROR", "--workspace", "acme", "--json", "repository,runNumber,stepName,text,logComplete", "--jq", ".[] | .runNumber"], env=env, capture_output=True, check=True)
        assert b"178" in result.stdout.splitlines(), result.stdout
        assert 'default_workspace = "acme"' in Path(directory, "config.toml").read_text()
        assert 'repository = "acme/service"' in Path(directory, "config.toml").read_text()
        before = len(requests)
        master, slave, original, process = launch(env, ["--offline"])
        try:
            wait_text(master, process, "Favorites")
            os.write(master, b"/not-found")
            wait_text(master, process, "No matching favorites")
            os.write(master, b"\x1b")
            wait_text(master, process, "Service")
            os.write(master, b"\x10")
            wait_text(master, process, "Quick open")
            os.write(master, b"service")
            wait_text(master, process, "Cached results only")
            os.write(master, b"\x1b")
            wait_text(master, process, "Favorites")
            os.write(master, b"*q")
            finish(master, slave, original, process, 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
        assert 'repository = "acme/service"' not in Path(directory, "config.toml").read_text()
        # Quitting during a blocked save still accepts Ctrl+C in raw terminal mode.
        master, slave, original, process = launch(env, ["--offline", "-R", "acme/service"])
        try:
            wait_text(master, process, "OFFLINE")
            with Path(directory, "config.lock").open("a+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                os.write(master, b"*q")
                wait_text(master, process, "Saving preferences before exit")
                os.write(master, b"\x03")
                finish(master, slave, original, process, 2)
                fcntl.flock(lock, fcntl.LOCK_UN)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
        for key, code in ((b"\x03", 2), (None, 2), (b"q", 0)):
            master, slave, original, process = launch(env, ["--offline", "-R", "acme/service"])
            try:
                wait_text(master, process, "OFFLINE")
                if key is None:
                    process.send_signal(signal.SIGINT)
                else:
                    os.write(master, key)
                finish(master, slave, original, process, code)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        assert len(requests) == before, "Offline mode performed an HTTP request"
        print(f"TUI navigation, child logs, favorites, quick/contextual search, pagination, default workspace, resize, q/Ctrl+C/SIGINT and offline checks passed; input response {response_ms} ms")
finally:
    server.shutdown()
    server.server_close()
