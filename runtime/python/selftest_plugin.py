# -*- coding: utf-8 -*-
"""
Minimal Flow Launcher plugin used by ``selftest.py``.

It answers every query with a single result whose title is the query text and
it pushes the same result to the host with ``UpdateResults`` so that the
bidirectional part of the protocol is covered as well.
"""

from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from python_plugin import FlowLauncher, Result  # noqa: E402


class SelfTestPlugin(FlowLauncher):
    """The smallest possible plugin: ``query("hello")`` -> one result."""

    def initialize(self, context):
        self.metadata = (context or {}).get("currentPluginMetadata", {})

    def query(self, query):
        text = str(query)
        results = [
            Result(
                title=text,
                sub_title="self test plugin",
                ico_path="",
                score=0,
            )
        ]

        # Push the results to the host while the query is still running.
        try:
            self.api.update_results(text, results, timeout=10.0)
        except Exception as error:  # pragma: no cover - depends on the host
            print("UpdateResults failed: %s" % error, file=sys.stderr)

        return results

    def context_menu(self, data):
        return [
            Result(
                title="context menu",
                sub_title="context data: %s" % (data,),
            )
        ]

    def close(self):
        print("closing", file=sys.stderr)


if __name__ == "__main__":
    SelfTestPlugin()
