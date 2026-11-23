# -*- coding: utf-8 -*-
"""
JSON-RPC 2.0 transport for the Flow Launcher V2 plugin protocol (Python side).

Wire format
-----------
Python plugins use the *newline delimited* flavour of JSON-RPC, exactly like the
official host does (``Flow.Launcher.Core/Plugin/PythonPluginV2.cs`` selects
``MessageHandlerType.NewLineDelimited`` which is backed by the ``StreamJsonRpc``
package, see ``Flow.Launcher.Core/Flow.Launcher.Core.csproj`` ->
``<PackageReference Include="StreamJsonRpc" Version="2.22.11" />``).

Every message is exactly **one line** of UTF-8 JSON terminated by ``\\n``::

    request   {"jsonrpc":"2.0","id":1,"method":"query","params":[...]}
    response  {"jsonrpc":"2.0","id":1,"result":{...}}
    error     {"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"...","data":null}}

Notes
-----
* ``stdout`` is the protocol channel, **never** write anything else to it.
  Diagnostics go to ``stderr``.
* Non ASCII characters are escaped (``ensure_ascii=True``) so that the stream
  stays pure ASCII and therefore independent of the console code page.

This module only implements the standard library.  It is a faithful, protocol
compatible re-implementation of the transport that the official host uses; the
public plugin API lives in ``python_plugin.py``.
"""

from __future__ import annotations

import atexit
import json
import queue
import sys
import threading
import time
import traceback
from typing import Any, Callable, Dict, List, Optional

JSONRPC_VERSION = "2.0"

# JSON-RPC 2.0 reserved error codes (https://www.jsonrpc.org/specification)
PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603


class _StopAfterResponse:
    """Sentinel returned by a request handler that wants the transport to shut
    down *after* the response frame has been written."""

    __slots__ = ()

    def __repr__(self) -> str:  # pragma: no cover - debug helper
        return "<STOP_AFTER_RESPONSE>"


#: Return this from a request handler to close the transport once the
#: response has been flushed to stdout.
STOP = _StopAfterResponse()


class JsonRpcError(Exception):
    """An error that is reported back to the peer as a JSON-RPC ``error`` object."""

    def __init__(self, code: int = INTERNAL_ERROR, message: str = "", data: Any = None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data

    def to_dict(self) -> Dict[str, Any]:
        error: Dict[str, Any] = {"code": self.code, "message": self.message}
        if self.data is not None:
            error["data"] = self.data
        return error


RequestHandler = Callable[[str, List[Any], Any], Any]


class JsonRpcTransport:
    """Newline delimited JSON-RPC 2.0 client/server running on top of two streams.

    The transport is bidirectional:

    * messages **read** from the reader stream are either *requests* coming
      from the host (they contain a ``method``) or *responses* to requests that
      the plugin itself sent (they contain ``result``/``error``).
    * messages **written** to the writer stream are responses to host requests
      or requests that the plugin sends to the host.
    """

    def __init__(self, reader: Any = None, writer: Any = None) -> None:
        self._reader = reader if reader is not None else getattr(sys.stdin, "buffer", sys.stdin)
        self._writer = writer if writer is not None else getattr(sys.stdout, "buffer", sys.stdout)
        self._writer_is_text = not hasattr(self._writer, "write") or isinstance(
            self._writer, type(sys.stdout)
        )

        self._handler: Optional[RequestHandler] = None
        self._requests: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self._pending: Dict[int, Any] = {}
        self._pending_lock = threading.Lock()
        self._write_lock = threading.Lock()
        self._next_request_id = 0
        self._stop = threading.Event()
        self._started = False
        self._closed = False
        self._on_stopped: Optional[Callable[[], None]] = None

    # ------------------------------------------------------------------
    # lifecycle
    # ------------------------------------------------------------------
    def start(self, handler: RequestHandler) -> None:
        """Start the reader/dispatch threads.

        ``handler(method, params, request_id)`` is called for every request that
        arrives from the host.  Its return value is sent back as ``result``.
        Raise :class:`JsonRpcError` to answer with an ``error`` object, or
        return :data:`STOP` to close the transport once the response was sent.
        """
        if self._started:
            raise RuntimeError("The transport has already been started")
        self._started = True
        self._handler = handler

        # Daemon threads do the actual work; the (non daemon) keep alive thread
        # makes sure the interpreter stays alive while the plugin is serving
        # the host, even though the plugin's ``__main__`` block already returned.
        threading.Thread(target=self._read_loop, name="flowlauncher-jsonrpc-reader", daemon=True).start()
        threading.Thread(target=self._dispatch_loop, name="flowlauncher-jsonrpc-dispatch", daemon=True).start()
        self._keep_alive = threading.Thread(target=self._keep_alive_loop, name="flowlauncher-jsonrpc-keepalive")
        self._keep_alive.daemon = False
        self._keep_alive.start()
        atexit.register(self.stop)

    def set_on_stopped(self, callback: Optional[Callable[[], None]]) -> None:
        """Register a callback that is invoked once the transport has stopped."""
        self._on_stopped = callback

    def _keep_alive_loop(self) -> None:
        """Keeps the interpreter alive while the plugin serves the host.

        The reader thread is a daemon thread that is blocked in ``readline()``
        on stdin.  Letting the interpreter finalize with such a thread alive
        aborts the process ("could not acquire lock for <_io.BufferedReader
        name='<stdin>'> at interpreter shutdown").  The plugin therefore ends
        the process itself once the host closed the connection.
        """
        self._stop.wait()
        # Give the dispatch thread the time to flush the last response.
        time.sleep(0.05)
        for stream in (sys.stdout, sys.stderr):
            try:
                stream.flush()
            except Exception:
                pass
        callback = self._on_stopped
        if callback is not None:
            try:
                callback()
            except Exception:
                pass

    def stop(self) -> None:
        """Stop the transport.  Safe to call more than once."""
        if self._closed:
            return
        self._closed = True
        self._stop.set()

    @property
    def stopped(self) -> bool:
        return self._stop.is_set()

    def wait_closed(self, timeout: Optional[float] = None) -> bool:
        """Block until the transport is closed."""
        return self._stop.wait(timeout)

    # ------------------------------------------------------------------
    # writing
    # ------------------------------------------------------------------
    def _write_message(self, message: Dict[str, Any]) -> None:
        payload = json.dumps(message, ensure_ascii=True, separators=(",", ":")) + "\n"
        with self._write_lock:
            try:
                if self._writer_is_text:
                    self._writer.write(payload)
                else:
                    self._writer.write(payload.encode("utf-8"))
                self._writer.flush()
            except (BrokenPipeError, ValueError, OSError):
                # The host is gone, there is nothing left to talk to.
                self.stop()

    def send_response(self, request_id: Any, result: Any = None) -> None:
        """Send a successful JSON-RPC response."""
        self._write_message({"jsonrpc": JSONRPC_VERSION, "id": request_id, "result": result})

    def send_error(self, request_id: Any, code: int, message: str, data: Any = None) -> None:
        """Send a JSON-RPC error response."""
        error: Dict[str, Any] = {"code": code, "message": message}
        if data is not None:
            error["data"] = data
        self._write_message({"jsonrpc": JSONRPC_VERSION, "id": request_id, "error": error})

    def send_request(self, method: str, params: Optional[List[Any]] = None, request_id: Any = None) -> Any:
        """Send a request to the host.  Returns the request id."""
        if request_id is None:
            with self._pending_lock:
                self._next_request_id += 1
                request_id = self._next_request_id
        self._write_message(
            {
                "jsonrpc": JSONRPC_VERSION,
                "id": request_id,
                "method": method,
                "params": list(params) if params is not None else [],
            }
        )
        return request_id

    # ------------------------------------------------------------------
    # calling the host
    # ------------------------------------------------------------------
    def call(self, method: str, params: Optional[List[Any]] = None, timeout: float = 30.0) -> Any:
        """Call a method on the host and wait for its response.

        Returns the ``result`` member of the host response.  Raises
        :class:`JsonRpcError` when the host answered with an error object and
        :class:`TimeoutError` when the host did not answer in time.
        """
        with self._pending_lock:
            self._next_request_id += 1
            request_id = self._next_request_id
            done = threading.Event()
            holder: Dict[str, Any] = {}
            self._pending[request_id] = (done, holder)

        try:
            self.send_request(method, params, request_id)
        except Exception:
            with self._pending_lock:
                self._pending.pop(request_id, None)
            raise

        if not done.wait(timeout):
            with self._pending_lock:
                self._pending.pop(request_id, None)
            raise TimeoutError("Timed out while waiting for the host to answer '%s'" % method)

        error = holder.get("error")
        if error is not None:
            raise JsonRpcError(
                error.get("code", INTERNAL_ERROR),
                error.get("message", ""),
                error.get("data"),
            )
        return holder.get("result")

    def call_async(self, method: str, params: Optional[List[Any]] = None) -> None:
        """Call a method on the host without waiting for the response."""
        self.send_request(method, params)

    # ------------------------------------------------------------------
    # reading
    # ------------------------------------------------------------------
    def _read_loop(self) -> None:
        while not self._stop.is_set():
            try:
                line = self._reader.readline()
            except (ValueError, OSError):
                break
            if not line:
                # EOF - the host closed the pipe.
                break
            if isinstance(line, bytes):
                try:
                    line = line.decode("utf-8")
                except UnicodeDecodeError:
                    continue
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except ValueError:
                self.send_error(None, PARSE_ERROR, "Parse error")
                continue
            if not isinstance(message, dict):
                self.send_error(None, INVALID_REQUEST, "Invalid Request")
                continue

            if "method" in message:
                self._requests.put(message)
            else:
                self._complete_pending(message)

        # The host is gone (EOF) - shut everything down.
        self.stop()

    def _complete_pending(self, message: Dict[str, Any]) -> None:
        request_id = message.get("id")
        with self._pending_lock:
            entry = self._pending.pop(request_id, None)
        if entry is None:
            return
        done, holder = entry
        if "error" in message and message["error"] is not None:
            holder["error"] = message["error"]
        else:
            holder["result"] = message.get("result")
        done.set()

    # ------------------------------------------------------------------
    # dispatching
    # ------------------------------------------------------------------
    def _dispatch_loop(self) -> None:
        while not self._stop.is_set():
            try:
                message = self._requests.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self._handle_request(message)
            except Exception:  # pragma: no cover - defensive
                traceback.print_exc(file=sys.stderr)
                self.stop()

    def _handle_request(self, message: Dict[str, Any]) -> None:
        request_id = message.get("id")
        method = message.get("method")
        params = message.get("params", [])
        if params is None:
            params = []
        elif isinstance(params, dict):
            params = [params]
        elif not isinstance(params, list):
            params = [params]

        if not isinstance(method, str):
            self.send_error(request_id, INVALID_REQUEST, "Invalid Request")
            return

        if self._handler is None:
            self.send_error(request_id, METHOD_NOT_FOUND, "Method not found: %s" % method)
            return

        try:
            result = self._handler(method, params, request_id)
        except JsonRpcError as error:
            self.send_error(request_id, error.code, error.message, error.data)
            return
        except TypeError as error:
            traceback.print_exc(file=sys.stderr)
            self.send_error(request_id, INVALID_PARAMS, str(error))
            return
        except Exception as error:
            traceback.print_exc(file=sys.stderr)
            self.send_error(
                request_id,
                INTERNAL_ERROR,
                "%s: %s" % (type(error).__name__, error),
            )
            return

        if result is STOP:
            self.send_response(request_id, None)
            self.stop()
            return

        self.send_response(request_id, result)
