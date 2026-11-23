# -*- coding: utf-8 -*-
"""
Compatibility shim for the original, official helper library.

The official helper library is published on PyPI under the name ``flowlauncher``
(https://pypi.org/project/flowlauncher/, source:
https://github.com/Flow-Launcher/Flow.Launcher.JsonRPC.Python, MIT licensed)
and plugins import it like this::

    from flowlauncher import FlowLauncher

This module makes that import work against the V2 (JSON-RPC 2.0) protocol.
It simply re-exports the runtime of ``python_plugin.py``, which implements the
very same API (``FlowLauncher`` base class with ``query``/``context_menu``/
``debug`` and the ``FlowLauncherAPI`` host API).

.. note:: A plugin that ships its **own** copy of the original (V1) helper
          library in its own ``lib`` directory keeps using that copy, because
          the plugin usually puts its own directories in front of ``sys.path``.
          Such a plugin speaks the old one-shot protocol and therefore does not
          work with the V2 host.
"""

from __future__ import annotations

from flowlauncher_api import FlowLauncherAPI  # noqa: F401
from python_plugin import (  # noqa: F401
    FlowLauncher,
    JsonRpcError,
    Plugin,
    Preview,
    Query,
    Result,
    main,
)

__all__ = [
    "FlowLauncher",
    "FlowLauncherAPI",
    "JsonRpcError",
    "Plugin",
    "Preview",
    "Query",
    "Result",
    "main",
]
