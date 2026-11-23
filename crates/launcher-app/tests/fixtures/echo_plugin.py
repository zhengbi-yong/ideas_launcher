"""A minimal plugin that speaks the launcher's JSON-RPC protocol.

It implements the protocol directly instead of using the runtime helpers, so
the test proves that the *host* side works on its own — no helper library in
the way.
"""

import json
import sys


def read_message():
    """Read one message from the host."""
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return None
    return json.loads(line)


def send(message):
    """Send one message to the host."""
    sys.stdout.write(json.dumps(message, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def main():
    while True:
        message = read_message()
        if message is None:
            break

        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") or []

        if method == "initialize":
            # The host tells us who it is.
            metadata = (params[0] if params else {}).get("currentPluginMetadata", {})
            send({"jsonrpc": "2.0", "id": request_id, "result": None})
            sys.stderr.write("initialized as " + str(metadata.get("id")) + "\n")

        elif method == "query":
            search = params[0] if params else ""
            # The search text may be a plain string or a full query object.
            if isinstance(search, dict):
                search = search.get("search", "")
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "id": request_id,
                        "result": [
                            {
                                "title": "echo: " + str(search),
                                "subTitle": "from the test plugin",
                                "score": 100,
                            }
                        ],
                        "settingsChange": None,
                        "debugMessage": "",
                    },
                }
            )

        elif method == "context_menu":
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "id": request_id,
                        "result": [{"title": "a context menu item"}],
                        "settingsChange": None,
                        "debugMessage": "",
                    },
                }
            )

        elif method == "close":
            send({"jsonrpc": "2.0", "id": request_id, "result": None})
            break

        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "no such method"},
                }
            )


if __name__ == "__main__":
    main()
