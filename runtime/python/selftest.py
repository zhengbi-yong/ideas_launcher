# -*- coding: utf-8 -*-
"""
End to end self test of the Flow Launcher Python runtime.

The script starts ``selftest_plugin.py`` as a child process, speaks the
newline delimited JSON-RPC 2.0 protocol with it (exactly like the host does)
and verifies the answers.

Usage::

    python selftest.py

Exit code ``0`` means PASS, ``1`` means FAIL.
"""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import threading
import time
from typing import Any, Dict, List, Optional

HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN = os.path.join(HERE, "selftest_plugin.py")
TIMEOUT = 20.0

INIT_CONTEXT: Dict[str, Any] = {
    "currentPluginMetadata": {
        "id": "Flow.Launcher.Plugin.SelfTest",
        "name": "Self Test",
        "author": "Flow Launcher",
        "version": "1.0.0",
        "language": "python",
        "description": "Self test plugin",
        "website": "https://github.com/Flow-Launcher/Flow.Launcher",
        "actionKeyword": "st",
        "actionKeywords": ["st"],
        "icoPath": "Images/app.png",
        "pluginDirectory": HERE,
        "pluginSettingsDirectoryPath": HERE,
        "pluginCacheDirectoryPath": HERE,
    },
    "api": {},
}

QUERY: Dict[str, Any] = {
    "originalQuery": "hello",
    "trimmedQuery": "hello",
    "isReQuery": False,
    "isHomeQuery": False,
    "search": "hello",
    "searchTerms": ["hello"],
    "actionKeyword": "",
}


class Report:
    """Tiny test reporter."""

    def __init__(self) -> None:
        self.failures: List[str] = []
        self.checks = 0

    def check(self, name: str, ok: bool, detail: str = "") -> bool:
        self.checks += 1
        if ok:
            print("  PASS  %s" % name)
        else:
            print("  FAIL  %s%s" % (name, (" - " + detail) if detail else ""))
            self.failures.append(name)
        return ok

    @property
    def ok(self) -> bool:
        return not self.failures


class Host:
    """A very small stand-in for the Flow Launcher host."""

    def __init__(self, args: Optional[List[str]] = None) -> None:
        env = os.environ.copy()
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        env["PYTHONPATH"] = HERE + os.pathsep + env.get("PYTHONPATH", "")

        self.process = subprocess.Popen(
            [sys.executable] + (args if args is not None else [PLUGIN]),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=HERE,
            env=env,
        )
        self.lines: "queue.Queue[str]" = queue.Queue()
        self.stderr_lines: List[str] = []
        self.requests: List[Dict[str, Any]] = []
        self._next_id = 0

        threading.Thread(target=self._read_stdout, name="selftest-stdout", daemon=True).start()
        threading.Thread(target=self._read_stderr, name="selftest-stderr", daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            self.lines.put(line.decode("utf-8", "replace"))

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self.stderr_lines.append(line.decode("utf-8", "replace"))

    # -- protocol ----------------------------------------------------
    def send(self, message: Dict[str, Any]) -> None:
        assert self.process.stdin is not None
        payload = (json.dumps(message) + "\n").encode("utf-8")
        self.process.stdin.write(payload)
        self.process.stdin.flush()

    def request(self, method: str, params: List[Any]) -> Dict[str, Any]:
        self._next_id += 1
        request_id = self._next_id
        self.send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        return self.wait_response(request_id)

    def wait_response(self, request_id: int, timeout: float = TIMEOUT) -> Dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("timeout while waiting for the response of %s" % request_id)
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty:
                raise TimeoutError("timeout while waiting for the response of %s" % request_id)

            line = line.strip()
            if not line:
                continue

            message = json.loads(line)
            if "method" in message:
                # The plugin calls the host -> answer the request.
                self.requests.append(message)
                self.send(
                    {"jsonrpc": "2.0", "id": message.get("id"), "result": None}
                )
                continue
            if message.get("id") == request_id:
                return message
            # A response to a request that we already handled -> ignore.

    def close_stdin(self) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except OSError:
            pass

    def kill(self) -> None:
        try:
            self.process.kill()
            self.process.wait(timeout=5)
        except Exception:
            pass


def _safe(text: str) -> str:
    """Escape a path so that it can be printed on any console code page."""
    return text.encode("unicode_escape").decode("ascii")


#: A plugin that uses the API of the *original* official helper library:
#: ``from flowlauncher import FlowLauncher`` and results as dictionaries with
#: PascalCase keys.
LEGACY_PLUGIN = """
import sys
sys.path.insert(0, {here!r})

from flowlauncher import FlowLauncher


class LegacyPlugin(FlowLauncher):
    def query(self, query):
        return [
            {{
                "Title": "legacy",
                "SubTitle": "official api",
                "IcoPath": "Images/app.png",
                "JsonRPCAction": {{"method": "open_url", "parameters": ["https://example.org"]}},
            }}
        ]

    def open_url(self, url):
        pass


LegacyPlugin()
""".format(here=HERE)


def run_legacy(report: Report) -> None:
    """Check that a plugin written against the original helper library works."""
    host = Host(args=["-c", LEGACY_PLUGIN])
    try:
        host.request("initialize", [INIT_CONTEXT])
        response = host.request("query", [QUERY, {}])
        results = ((response.get("result") or {}).get("result")) or []
        report.check(
            "a plugin using the original API returns one result",
            len(results) == 1,
            json.dumps(response),
        )
        if results:
            report.check(
                "the PascalCase 'Title' is sent as 'title'",
                results[0].get("title") == "legacy",
                json.dumps(results),
            )
            report.check(
                "the PascalCase 'SubTitle' is sent as 'subTitle'",
                results[0].get("subTitle") == "official api",
                json.dumps(results),
            )
            report.check(
                "the 'JsonRPCAction' is sent as 'jsonRPCAction'",
                results[0].get("jsonRPCAction", {}).get("method") == "open_url",
                json.dumps(results),
            )
        host.request("close", [])
    except Exception as error:  # pragma: no cover - only on a broken runtime
        report.check(
            "a plugin using the original API works",
            False,
            "%s: %s" % (type(error).__name__, error),
        )
    finally:
        if host.process.poll() is None:
            host.kill()


def run(report: Report) -> None:
    host = Host()
    try:
        # ----------------------------------------------------------
        print("1) initialize")
        response = host.request("initialize", [INIT_CONTEXT])
        report.check(
            "initialize is answered with a result",
            response.get("id") == 1 and "result" in response,
            json.dumps(response),
        )

        # ----------------------------------------------------------
        print("2) query")
        response = host.request("query", [QUERY, {}])
        report.check(
            "query is answered with id 2",
            response.get("id") == 2 and "result" in response,
            json.dumps(response),
        )
        payload = response.get("result") or {}
        report.check(
            "query response has result/settingsChange/debugMessage",
            "result" in payload and "settingsChange" in payload and "debugMessage" in payload,
            json.dumps(payload),
        )
        results = payload.get("result") or []
        report.check("query returns exactly one result", len(results) == 1, json.dumps(results))
        title = results[0].get("title") if results else None
        report.check("the title of the result is 'hello'", title == "hello", repr(title))
        report.check(
            "the result uses camelCase field names",
            bool(results) and "subTitle" in results[0] and "Title" not in results[0],
            json.dumps(results),
        )

        # ----------------------------------------------------------
        print("3) UpdateResults (plugin -> host)")
        updates = [r for r in host.requests if r.get("method") == "UpdateResults"]
        report.check(
            "the plugin called UpdateResults",
            len(updates) == 1,
            json.dumps(host.requests),
        )
        if updates:
            params = updates[0].get("params") or []
            report.check(
                "UpdateResults sends [trimmedQuery, response]",
                len(params) == 2 and params[0] == "hello",
                json.dumps(params),
            )
            pushed = (params[1] or {}).get("result") or [] if len(params) > 1 else []
            report.check(
                "the pushed result has the title 'hello'",
                bool(pushed) and pushed[0].get("title") == "hello",
                json.dumps(pushed),
            )

        # ----------------------------------------------------------
        print("4) context_menu")
        response = host.request("context_menu", [{"some": "data"}])
        menu = ((response.get("result") or {}).get("result")) or []
        report.check(
            "context_menu returns one result",
            response.get("id") == 3 and len(menu) == 1,
            json.dumps(response),
        )

        # ----------------------------------------------------------
        print("5) close")
        response = host.request("close", [])
        report.check(
            "close is answered",
            response.get("id") == 4 and "result" in response,
            json.dumps(response),
        )
        exit_code = host.process.wait(timeout=TIMEOUT)
        report.check("the plugin exits after close", exit_code == 0, "exit code %s" % exit_code)
    except Exception as error:  # pragma: no cover - only on a broken runtime
        report.check("the plugin answered every request", False, "%s: %s" % (type(error).__name__, error))
    finally:
        if host.process.poll() is None:
            host.kill()

    if host.stderr_lines:
        print("   plugin stderr:")
        for line in host.stderr_lines:
            print("     | %s" % line.rstrip())


def main() -> int:
    report = Report()
    print("Flow Launcher Python runtime - self test")
    print("plugin: %s" % _safe(PLUGIN))
    print("")
    run(report)
    print("")
    print("6) original official API (from flowlauncher import FlowLauncher)")
    run_legacy(report)
    print("")
    if report.ok:
        print("PASS  (%d checks)" % report.checks)
        return 0
    print("FAIL  (%d of %d checks failed)" % (len(report.failures), report.checks))
    for failure in report.failures:
        print("  - %s" % failure)
    return 1


if __name__ == "__main__":
    sys.exit(main())
