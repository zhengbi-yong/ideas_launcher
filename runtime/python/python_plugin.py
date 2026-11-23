# -*- coding: utf-8 -*-
"""
Flow Launcher Python plugin runtime (JSON-RPC 2.0 / V2 protocol).

This is the runtime library that a Flow Launcher Python plugin needs.  It is a
protocol compatible re-implementation of the helper library that the official
host ships in its ``JsonRPC`` folder, ported to the JSON-RPC 2.0 line
delimited protocol used by the V2 host
(``Flow.Launcher.Core/Plugin/PythonPluginV2.cs``,
``Flow.Launcher.Core/Plugin/JsonRPCPluginV2.cs``).

Public API
----------
``Plugin`` (also exported as ``FlowLauncher``, the name used by the original
official helper library) is the base class every plugin derives from::

    from python_plugin import FlowLauncher

    class MyPlugin(FlowLauncher):
        def query(self, query):
            return [{"Title": "hello", "SubTitle": "world"}]

    if __name__ == "__main__":
        MyPlugin()

The original helper library is published on PyPI as ``flowlauncher`` and comes
from https://github.com/Flow-Launcher/Flow.Launcher.JsonRPC.Python (MIT).
The class name, the method names, the callback signatures and the way results
are constructed (a list of dictionaries) are kept identical here.

Protocol
--------
* ``stdout`` carries the protocol, one JSON message per line.
* The host calls ``initialize(context)``, ``query(query, settings)``,
  ``context_menu(contextData)``, ``reload_data(context)`` and ``close()``.
* The plugin answers with ``{"id":.., "result":[..], "settingsChange":..,
  "debugMessage":..}`` for ``query``/``context_menu``.
* The plugin may call the host (``UpdateResults``, ``ChangeQuery``,
  ``ShowMsg``, ...) through :class:`FlowLauncherAPI`.
"""

from __future__ import annotations

import functools
import os
import sys
import threading
from dataclasses import dataclass, field, fields
from typing import Any, Dict, List, Optional

# Make sure the sibling modules of this file can always be imported, no matter
# how the plugin was started (PYTHONPATH, sys.path manipulation, ...).
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.append(_HERE)

from flowlauncher_api import FlowLauncherAPI  # noqa: E402
from jsonrpc_transport import (  # noqa: E402
    INTERNAL_ERROR,
    METHOD_NOT_FOUND,
    STOP,
    JsonRpcError,
    JsonRpcTransport,
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


# ======================================================================
# Result / Preview
# ======================================================================
@dataclass
class Preview:
    """Preview panel information (``Flow.Launcher.Plugin/Result.cs`` -> ``PreviewInfo``)."""

    preview_image_path: Optional[str] = None
    is_media: bool = False
    description: Optional[str] = None
    file_path: Optional[str] = None
    content_type: str = "imageWithText"

    def to_dict(self) -> Dict[str, Any]:
        data: Dict[str, Any] = {}
        if self.preview_image_path is not None:
            data["previewImagePath"] = self.preview_image_path
        if self.is_media:
            data["isMedia"] = True
        if self.description is not None:
            data["description"] = self.description
        if self.file_path is not None:
            data["filePath"] = self.file_path
        if self.content_type is not None:
            data["contentType"] = self.content_type
        return data


#: python attribute name -> JSON name of a :class:`Result` field.
_RESULT_FIELDS = (
    ("title", "title"),
    ("sub_title", "subTitle"),
    ("title_tooltip", "titleToolTip"),
    ("sub_title_tooltip", "subTitleToolTip"),
    ("ico_path", "icoPath"),
    ("badge_ico_path", "badgeIcoPath"),
    ("glyph", "glyph"),
    ("action_keyword_assigned", "actionKeywordAssigned"),
    ("copy_text", "copyText"),
    ("auto_complete_text", "autoCompleteText"),
    ("score", "score"),
    ("title_highlight_data", "titleHighlightData"),
    ("context_data", "contextData"),
    ("plugin_id", "pluginID"),
    ("progress_bar", "progressBar"),
    ("progress_bar_color", "progressBarColor"),
    ("preview", "preview"),
    ("preview_visibility", "previewVisibility"),
    ("add_selected_count", "addSelectedCount"),
    ("record_key", "recordKey"),
    ("show_badge", "showBadge"),
    ("query_suggestion_text", "querySuggestionText"),
    ("rounded_icon", "roundedIcon"),
    ("action", "jsonRPCAction"),
    ("settings_change", "settingsChange"),
)

#: case insensitive lookup of the JSON names of a result.
_RESULT_KEYS = {json_name.lower(): json_name for _, json_name in _RESULT_FIELDS}

#: case insensitive lookup for the members of the ``preview`` object.
_PREVIEW_KEYS = {
    "previewimagepath": "previewImagePath",
    "ismedia": "isMedia",
    "description": "description",
    "filepath": "filePath",
    "contenttype": "contentType",
}

#: case insensitive lookup for the members of the ``jsonRPCAction`` object.
_ACTION_KEYS = {
    "id": "id",
    "method": "method",
    "parameters": "parameters",
    "settings": "settings",
    "donthideafteraction": "dontHideAfterAction",
}


@dataclass
class Result:
    """A single result row.

    The official helper library has no ``Result`` class - a result is a plain
    dictionary.  This class is a convenience wrapper around exactly that
    dictionary; both forms are accepted by :meth:`Plugin.query`.
    """

    title: Optional[str] = None
    sub_title: Optional[str] = None
    title_tooltip: Optional[str] = None
    sub_title_tooltip: Optional[str] = None
    ico_path: Optional[str] = None
    badge_ico_path: Optional[str] = None
    glyph: Optional[Any] = None
    action_keyword_assigned: Optional[str] = None
    copy_text: Optional[str] = None
    auto_complete_text: Optional[str] = None
    score: Optional[int] = None
    title_highlight_data: Optional[List[int]] = None
    context_data: Optional[Any] = None
    plugin_id: Optional[str] = None
    progress_bar: Optional[int] = None
    progress_bar_color: Optional[str] = None
    preview: Optional[Any] = None
    preview_visibility: Optional[str] = None
    add_selected_count: Optional[bool] = None
    record_key: Optional[str] = None
    show_badge: Optional[bool] = None
    query_suggestion_text: Optional[str] = None
    rounded_icon: Optional[bool] = None
    action: Optional[Any] = None
    settings_change: Optional[Any] = None

    def to_dict(self) -> Dict[str, Any]:
        """Serialize to the JSON object that the host expects."""
        data: Dict[str, Any] = {}
        for attribute, json_name in _RESULT_FIELDS:
            value = getattr(self, attribute)
            if value is None:
                continue
            data[json_name] = _normalize_value(json_name, value)
        return data

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "Result":
        """Create a result from a dictionary that uses any casing."""
        instance = cls()
        for key, value in data.items():
            attribute = _attribute_for(key)
            if attribute is not None:
                setattr(instance, attribute, value)
        return instance


def _attribute_for(key: str) -> Optional[str]:
    """Map a (possibly PascalCase or camelCase) key to a Result attribute."""
    for attribute, json_name in _RESULT_FIELDS:
        if key.lower() == json_name.lower():
            return attribute
    return None


def _normalize_value(json_name: str, value: Any) -> Any:
    if json_name == "preview":
        return _normalize_preview(value)
    if json_name == "jsonRPCAction":
        return _normalize_action(value)
    if json_name == "previewVisibility" and isinstance(value, str):
        return value.lower()
    return value


def _normalize_preview(value: Any) -> Any:
    if isinstance(value, Preview):
        return value.to_dict()
    if isinstance(value, dict):
        return {
            _PREVIEW_KEYS.get(key.lower(), key): item
            for key, item in value.items()
        }
    return value


def _normalize_action(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            _ACTION_KEYS.get(key.lower(), key): item
            for key, item in value.items()
        }
    return value


def _normalize_results(results: Any) -> List[Dict[str, Any]]:
    """Normalize whatever a plugin returned into a list of result objects."""
    if results is None:
        return []
    if isinstance(results, (dict, Result)):
        results = [results]
    if not isinstance(results, (list, tuple)):
        raise TypeError(
            "A plugin must return a list of results, got %r" % type(results).__name__
        )

    normalized: List[Dict[str, Any]] = []
    for result in results:
        if isinstance(result, Result):
            normalized.append(result.to_dict())
        elif isinstance(result, dict):
            item: Dict[str, Any] = {}
            for key, value in result.items():
                json_name = _RESULT_KEYS.get(key.lower(), key)
                item[json_name] = _normalize_value(json_name, value)
            normalized.append(item)
        else:
            raise TypeError(
                "A result must be a dictionary or a Result instance, got %r"
                % type(result).__name__
            )
    return normalized


# ======================================================================
# Query
# ======================================================================
class Query(str):
    """The query that the host sent to :meth:`Plugin.query`.

    For compatibility with the original helper library the object *is* the
    query text (a ``str``).  The V2 protocol sends a richer object, all of its
    members are available as attributes and as dictionary items::

        query                -> '<trimmed query>'
        query.search         -> the search part (without the action keyword)
        query["searchTerms"] -> the search terms
        query.trimmed_query  -> snake_case alias

    The members are the ones of ``Flow.Launcher.Plugin/Query.cs``:
    ``originalQuery``, ``trimmedQuery``, ``isReQuery``, ``isHomeQuery``,
    ``search``, ``searchTerms`` and ``actionKeyword``.
    """

    _MEMBERS = (
        "originalQuery",
        "trimmedQuery",
        "isReQuery",
        "isHomeQuery",
        "search",
        "searchTerms",
        "actionKeyword",
    )

    _ALIASES = {
        "original_query": "originalQuery",
        "trimmed_query": "trimmedQuery",
        "is_requery": "isReQuery",
        "is_home_query": "isHomeQuery",
        "search_terms": "searchTerms",
        "action_keyword": "actionKeyword",
    }

    def __new__(cls, data: Any = None, **kwargs: Any) -> "Query":
        raw: Dict[str, Any] = {}
        if isinstance(data, dict):
            raw.update(data)
        elif isinstance(data, str):
            raw["trimmedQuery"] = data
            raw["search"] = data
        raw.update(kwargs)

        members: Dict[str, Any] = {}
        for key, value in raw.items():
            name = cls._ALIASES.get(key, key)
            for member in cls._MEMBERS:
                if member.lower() == name.lower():
                    members[member] = value
                    break

        text = members.get("trimmedQuery") or members.get("search") or ""
        obj = super().__new__(cls, text)
        obj._members = members  # type: ignore[attr-defined]
        return obj

    # -- attribute access ---------------------------------------------
    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)
        members = self.__dict__.get("_members", {})
        if name in members:
            return members[name]
        alias = Query._ALIASES.get(name)
        if alias and alias in members:
            return members[alias]
        for member, value in members.items():
            if member.lower() == name.lower():
                return value
        raise AttributeError(name)

    # -- dictionary access --------------------------------------------
    def __getitem__(self, key: Any) -> Any:
        if isinstance(key, str):
            members = self.__dict__.get("_members", {})
            if key in members:
                return members[key]
            alias = Query._ALIASES.get(key)
            if alias and alias in members:
                return members[alias]
            for member, value in members.items():
                if member.lower() == key.lower():
                    return value
        return str.__getitem__(self, key)

    def get(self, key: str, default: Any = None) -> Any:
        try:
            return self[key]
        except KeyError:
            return default

    def to_dict(self) -> Dict[str, Any]:
        return dict(self.__dict__.get("_members", {}))


# ======================================================================
# Plugin
# ======================================================================
class Plugin:
    """Base class of a Flow Launcher Python plugin.

    Subclasses override :meth:`query` and (optionally)
    :meth:`context_menu`.  The object starts serving the host as soon as it is
    constructed; the host then drives the plugin with ``initialize``,
    ``query``, ``context_menu``, ``reload_data`` and ``close``.
    """

    #: End the process when the host closed the connection.
    #:
    #: The plugin runs in its own process.  When the host sends ``close`` (or
    #: closes the pipe) the runtime terminates the process, because the
    #: reader thread is blocked on stdin and the interpreter would otherwise
    #: abort while finalizing.  Set this to ``False`` when the runtime is used
    #: inside a larger process.
    exit_process_when_closed = True

    def __init__(self) -> None:
        self._transport = JsonRpcTransport()
        self._api = FlowLauncherAPI(self._transport)
        FlowLauncherAPI._bind(self._transport)

        #: Message shown by the host when the query is answered (original API).
        self.debugMessage = ""

        #: The ``PluginInitContext`` the host sent with ``initialize``.
        self.context: Dict[str, Any] = {}

        #: ``currentPluginMetadata`` of the ``PluginInitContext``.
        self.current_plugin_metadata: Dict[str, Any] = {}

        #: The settings the host sent along with the current query.
        self.settings: Dict[str, Any] = {}

        #: Value for the ``settingsChange`` member of the query response.
        self.settings_change: Optional[Dict[str, Any]] = None

        self._startup = threading.Event()
        self._transport.set_on_stopped(self._on_transport_stopped)
        self._transport.start(self._handle_request)

        # When the subclass does not define its own ``__init__`` we are ready
        # right away, otherwise the wrapper installed by ``__init_subclass__``
        # signals readiness as soon as the subclass finished initialising.
        if not getattr(type(self).__init__, "_flow_launcher_wrapped", False):
            self._startup.set()

    def __init_subclass__(cls, **kwargs: Any) -> None:
        super().__init_subclass__(**kwargs)
        original = cls.__dict__.get("__init__")
        if original is None or getattr(original, "_flow_launcher_wrapped", False):
            return

        @functools.wraps(original)
        def __init__(self: "Plugin", *args: Any, **kwargs: Any) -> None:
            try:
                original(self, *args, **kwargs)
            finally:
                event = self.__dict__.get("_startup")
                if event is not None:
                    event.set()

        __init__._flow_launcher_wrapped = True  # type: ignore[attr-defined]
        cls.__init__ = __init__  # type: ignore[assignment]

    # ------------------------------------------------------------------
    # host API
    # ------------------------------------------------------------------
    @property
    def api(self) -> FlowLauncherAPI:
        """The Flow Launcher host API of this plugin instance."""
        return self._api

    # ------------------------------------------------------------------
    # plugin callbacks
    # ------------------------------------------------------------------
    def query(self, param: str = "") -> list:
        """sub class need to override this method"""
        return []

    def context_menu(self, data) -> list:
        """optional context menu entries for a result"""
        return []

    def debug(self, msg: str) -> None:
        """alert msg"""
        self.debugMessage = msg

    def initialize(self, context: Dict[str, Any]) -> None:
        """Called by the host once, with the ``PluginInitContext``.

        Do the setup of the plugin here (read the metadata, prepare caches,
        ...).  This is the V2 counterpart of doing the setup in ``__init__``.
        """
        return None

    def reload_data(self, context: Dict[str, Any]) -> None:
        """Called by the host when the plugin data should be reloaded."""
        return None

    def close(self) -> None:
        """Called by the host right before the plugin is shut down."""
        return None

    # ------------------------------------------------------------------
    # protocol
    # ------------------------------------------------------------------
    def _handle_request(self, method: str, params: List[Any], request_id: Any) -> Any:
        # Wait until the plugin finished its own initialisation.
        self._startup.wait()

        if method == "initialize":
            context = params[0] if params else None
            self.context = context if isinstance(context, dict) else {}
            self.current_plugin_metadata = _lookup(self.context, "currentPluginMetadata") or {}
            self.initialize(self.context)
            return {}

        if method == "query":
            # The host sends the query object; the plugin sees a ``Query``,
            # which *is* the query text (like in the original helper library).
            query = Query(params[0]) if params else Query("")
            if len(params) > 1 and isinstance(params[1], dict):
                self.settings = params[1]
            results = self.query(query)
            return self._build_response(request_id, results)

        if method == "context_menu":
            data = params[0] if params else None
            results = self.context_menu(data)
            return self._build_response(request_id, results)

        if method == "reload_data":
            context = params[0] if params else self.context
            self.reload_data(context if isinstance(context, dict) else {})
            return {}

        if method == "close":
            self.close()
            return STOP

        # Everything else is an action of the plugin: the host calls a method
        # of the plugin instance (the "JsonRPCAction" of the official helper).
        action = getattr(self, method, None)
        if callable(action):
            result = action(*params)
            return {"hide": True} if result is None else result

        raise JsonRpcError(METHOD_NOT_FOUND, "Method not found: %s" % method)

    def _build_response(self, request_id: Any, results: Any) -> Dict[str, Any]:
        return {
            "id": request_id,
            "result": _normalize_results(results),
            "settingsChange": self.settings_change,
            "debugMessage": self.debugMessage or "",
        }

    # ------------------------------------------------------------------
    # lifecycle helpers
    # ------------------------------------------------------------------
    def _on_transport_stopped(self) -> None:
        """Called after the host closed the connection."""
        if self.exit_process_when_closed:
            os._exit(0)

    def stop(self) -> None:
        """Stop serving the host (the process exits afterwards)."""
        self._transport.stop()

    def wait_closed(self, timeout: Optional[float] = None) -> bool:
        """Block until the host closed the connection."""
        return self._transport.wait_closed(timeout)


def _lookup(mapping: Dict[str, Any], name: str) -> Any:
    """Case insensitive dictionary lookup."""
    if not isinstance(mapping, dict):
        return None
    if name in mapping:
        return mapping[name]
    for key, value in mapping.items():
        if isinstance(key, str) and key.lower() == name.lower():
            return value
    return None


#: The original official helper library names the base class ``FlowLauncher``.
FlowLauncher = Plugin


def main(plugin: Optional[Plugin] = None) -> None:
    """Entry point helper: run ``plugin`` until the host closes the connection."""
    instance = plugin if plugin is not None else Plugin()
    instance.wait_closed()
