# -*- coding: utf-8 -*-
"""
Host API for Flow Launcher Python plugins (V2 / JSON-RPC 2.0).

The original, official helper library (``flowlauncher`` on PyPI, published from
https://github.com/Flow-Launcher/Flow.Launcher.JsonRPC.Python, MIT licensed)
exposes the host API through the ``FlowLauncherAPI`` class with **class
methods** that simply print a JSON-RPC frame to stdout::

    FlowLauncherAPI.change_query("new query")
    FlowLauncherAPI.show_msg("title", "sub title")

This module keeps that exact API (same class name, same method names, same
argument order) but routes the calls through the real JSON-RPC 2.0 channel so
that they work with the V2 protocol used by the host.

The V2 host exposes the API implemented in
``Flow.Launcher.Core/Plugin/JsonRPCV2Models/JsonRPCPublicAPI.cs``; the method
names below are exactly the names used on the wire.  ``UpdateResults`` is
registered by the host in ``JsonRPCPluginV2.SetupJsonRPC()``::

    RPC.AddLocalRpcMethod("UpdateResults", new Action<string, JsonRPCQueryResponseModel>(...))

Both call styles work::

    from flowlauncher_api import FlowLauncherAPI
    FlowLauncherAPI.show_msg("hi")          # class style  (original API)
    self.api.show_msg("hi")                 # instance style (plugin instance)
"""

from __future__ import annotations

from typing import Any, Callable, Dict, List, Optional

from jsonrpc_transport import JsonRpcError, JsonRpcTransport


class _host_method:
    """Descriptor that lets the API methods be used both as class methods and
    as instance methods.

    * class access  -> uses the transport of the currently running plugin
    * instance access -> uses the transport of that plugin instance
    """

    def __init__(self, func: Callable[..., Any]) -> None:
        self.func = func
        self.__doc__ = func.__doc__
        self.__name__ = getattr(func, "__name__", "host_method")

    def __get__(self, obj: Any, objtype: Any = None) -> Callable[..., Any]:
        transport = None
        if obj is not None:
            transport = getattr(obj, "_transport", None)
        if transport is None:
            transport = getattr(objtype, "_default_transport", None)
        if transport is None:
            raise RuntimeError(
                "No Flow Launcher host is connected. "
                "Host API calls are only available while the plugin runs inside the host."
            )

        def bound(*args: Any, **kwargs: Any) -> Any:
            return self.func(transport, *args, **kwargs)

        bound.__doc__ = self.__doc__
        bound.__name__ = self.__name__
        return bound


class FlowLauncherAPI:
    """Call the Flow Launcher host API from inside a plugin."""

    #: Transport of the plugin instance that is currently running (if any).
    _default_transport: Optional[JsonRpcTransport] = None

    def __init__(self, transport: Optional[JsonRpcTransport] = None) -> None:
        self._transport = transport

    @classmethod
    def _bind(cls, transport: JsonRpcTransport) -> None:
        """Remember the transport that is used for class level calls."""
        cls._default_transport = transport

    # ------------------------------------------------------------------
    # low level helpers
    # ------------------------------------------------------------------
    @_host_method
    def call(transport: JsonRpcTransport, method: str, *params: Any, timeout: float = 30.0) -> Any:
        """Call an arbitrary method of the host and return its result."""
        return transport.call(method, list(params), timeout=timeout)

    @_host_method
    def update_results(
        transport: JsonRpcTransport,
        trimmed_query: str,
        results: Any,
        settings_change: Any = None,
        debug_message: str = "",
        timeout: float = 30.0,
    ) -> Any:
        """Push results to the host while a query is still running.

        ``params`` are ``[trimmedQuery, response]`` where ``response`` has the
        shape ``{"id":.., "result":[..], "settingsChange":.., "debugMessage":..}``.
        """
        # Imported here to avoid a circular import (python_plugin imports this
        # module).
        from python_plugin import _normalize_results

        response = {
            "id": 0,
            "result": _normalize_results(results),
            "settingsChange": settings_change,
            "debugMessage": debug_message or "",
        }
        return transport.call("UpdateResults", [trimmed_query, response], timeout=timeout)

    # ------------------------------------------------------------------
    # original (V1) API of the official helper library
    # ------------------------------------------------------------------
    @_host_method
    def change_query(transport: JsonRpcTransport, query: str, requery: bool = False) -> Any:
        """Change the query of the host (original API: ``change_query``)."""
        return transport.call("ChangeQuery", [query, requery])

    @_host_method
    def shell_run(transport: JsonRpcTransport, cmd: str, filename: str = "cmd.exe") -> Any:
        """Run a shell command (original API: ``shell_run``)."""
        return transport.call("ShellRun", [cmd, filename])

    @_host_method
    def close_app(transport: JsonRpcTransport) -> Any:
        """Close Flow Launcher (original API: ``close_app``).

        .. note:: The V2 host does not expose a ``CloseApp`` method, the call
                 is answered with a JSON-RPC "method not found" error.
        """
        return transport.call("CloseApp", [])

    @_host_method
    def hide_app(transport: JsonRpcTransport) -> Any:
        """Hide the main window (original API: ``hide_app``)."""
        return transport.call("HideMainWindow", [])

    @_host_method
    def show_app(transport: JsonRpcTransport) -> Any:
        """Show the main window (original API: ``show_app``)."""
        return transport.call("ShowMainWindow", [])

    @_host_method
    def show_msg(transport: JsonRpcTransport, title: str, sub_title: str = "", ico_path: str = "") -> Any:
        """Show a message box (original API: ``show_msg``)."""
        return transport.call("ShowMsg", [title, sub_title, ico_path])

    @_host_method
    def open_setting_dialog(transport: JsonRpcTransport) -> Any:
        """Open the settings dialog (original API: ``open_setting_dialog``)."""
        return transport.call("OpenSettingDialog", [])

    @_host_method
    def start_loadingbar(transport: JsonRpcTransport) -> Any:
        """Start the loading animation (original API: ``start_loadingbar``)."""
        return transport.call("StartLoadingBar", [])

    @_host_method
    def stop_loadingbar(transport: JsonRpcTransport) -> Any:
        """Stop the loading animation (original API: ``stop_loadingbar``)."""
        return transport.call("StopLoadingBar", [])

    @_host_method
    def reload_plugins(transport: JsonRpcTransport) -> Any:
        """Reload the data of all plugins (original API: ``reload_plugins``)."""
        return transport.call("ReloadAllPluginDataAsync", [])

    # ------------------------------------------------------------------
    # V2 API (names taken from Flow.Launcher.Core/Plugin/JsonRPCV2Models/
    # JsonRPCPublicAPI.cs)
    # ------------------------------------------------------------------
    @_host_method
    def restart_app(transport: JsonRpcTransport) -> Any:
        """Restart Flow Launcher."""
        return transport.call("RestartApp", [])

    @_host_method
    def copy_to_clipboard(
        transport: JsonRpcTransport,
        text: str,
        direct_copy: bool = False,
        show_default_notification: bool = True,
    ) -> Any:
        """Copy ``text`` to the clipboard."""
        return transport.call("CopyToClipboard", [text, direct_copy, show_default_notification])

    @_host_method
    def save_app_all_settings(transport: JsonRpcTransport) -> Any:
        """Save the settings of Flow Launcher."""
        return transport.call("SaveAppAllSettings", [])

    @_host_method
    def save_plugin_settings(transport: JsonRpcTransport) -> Any:
        """Save the settings of this plugin."""
        return transport.call("SavePluginSettings", [])

    @_host_method
    def save_plugin_caches(transport: JsonRpcTransport) -> Any:
        """Save the caches of this plugin."""
        return transport.call("SavePluginCaches", [])

    @_host_method
    def check_for_new_update(transport: JsonRpcTransport) -> Any:
        """Ask the host to check for a new version."""
        return transport.call("CheckForNewUpdate", [])

    @_host_method
    def show_msg_error(transport: JsonRpcTransport, title: str, sub_title: str = "") -> Any:
        """Show an error message box."""
        return transport.call("ShowMsgError", [title, sub_title])

    @_host_method
    def is_main_window_visible(transport: JsonRpcTransport) -> Any:
        """Return whether the main window is currently visible."""
        return transport.call("IsMainWindowVisible", [])

    @_host_method
    def open_plugin_settings_window(transport: JsonRpcTransport, plugin_id: str) -> Any:
        """Open the settings window of the plugin with ``plugin_id``."""
        return transport.call("OpenPluginSettingsWindow", [plugin_id])

    @_host_method
    def get_translation(transport: JsonRpcTransport, key: str) -> Any:
        """Return the translated text for ``key``."""
        return transport.call("GetTranslation", [key])

    @_host_method
    def get_all_plugins(transport: JsonRpcTransport) -> Any:
        """Return the list of all installed plugins."""
        return transport.call("GetAllPlugins", [])

    @_host_method
    def fuzzy_search(transport: JsonRpcTransport, query: str, string_to_compare: str) -> Any:
        """Run the fuzzy search of the host and return the match result."""
        return transport.call("FuzzySearch", [query, string_to_compare])

    @_host_method
    def http_get_string_async(transport: JsonRpcTransport, url: str) -> Any:
        """Download ``url`` and return its content as a string."""
        return transport.call("HttpGetStringAsync", [url])

    @_host_method
    def http_get_stream_async(transport: JsonRpcTransport, url: str) -> Any:
        """Download ``url`` and return its content as a stream."""
        return transport.call("HttpGetStreamAsync", [url])

    @_host_method
    def http_download_async(transport: JsonRpcTransport, url: str, file_path: str) -> Any:
        """Download ``url`` into ``file_path``."""
        return transport.call("HttpDownloadAsync", [url, file_path])

    @_host_method
    def add_action_keyword(transport: JsonRpcTransport, plugin_id: str, new_action_keyword: str) -> Any:
        """Add an action keyword to a plugin."""
        return transport.call("AddActionKeyword", [plugin_id, new_action_keyword])

    @_host_method
    def remove_action_keyword(transport: JsonRpcTransport, plugin_id: str, old_action_keyword: str) -> Any:
        """Remove an action keyword from a plugin."""
        return transport.call("RemoveActionKeyword", [plugin_id, old_action_keyword])

    @_host_method
    def action_keyword_assigned(transport: JsonRpcTransport, action_keyword: str) -> Any:
        """Return whether ``action_keyword`` is already used by a plugin."""
        return transport.call("ActionKeywordAssigned", [action_keyword])

    @_host_method
    def log_debug(transport: JsonRpcTransport, class_name: str, message: str, method_name: str = "") -> Any:
        """Write a debug message to the log of the host."""
        return transport.call("LogDebug", [class_name, message, method_name])

    @_host_method
    def log_info(transport: JsonRpcTransport, class_name: str, message: str, method_name: str = "") -> Any:
        """Write an info message to the log of the host."""
        return transport.call("LogInfo", [class_name, message, method_name])

    @_host_method
    def log_warn(transport: JsonRpcTransport, class_name: str, message: str, method_name: str = "") -> Any:
        """Write a warning message to the log of the host."""
        return transport.call("LogWarn", [class_name, message, method_name])

    @_host_method
    def log_error(transport: JsonRpcTransport, class_name: str, message: str, method_name: str = "") -> Any:
        """Write an error message to the log of the host."""
        return transport.call("LogError", [class_name, message, method_name])

    @_host_method
    def open_directory(
        transport: JsonRpcTransport, directory_path: str, file_name_or_file_path: Optional[str] = None
    ) -> Any:
        """Open a directory (and optionally a file inside of it)."""
        return transport.call("OpenDirectory", [directory_path, file_name_or_file_path])

    @_host_method
    def open_url(transport: JsonRpcTransport, url: str, in_private: Optional[bool] = None) -> Any:
        """Open a URL in the default browser."""
        return transport.call("OpenUrl", [url, in_private])

    @_host_method
    def open_app_uri(transport: JsonRpcTransport, app_uri: str) -> Any:
        """Open an application URI (e.g. ``ms-settings:``)."""
        return transport.call("OpenAppUri", [app_uri])

    @_host_method
    def back_to_query_results(transport: JsonRpcTransport) -> Any:
        """Close the preview pane and show the query results again."""
        return transport.call("BackToQueryResults", [])

    @_host_method
    def start_loading_bar(transport: JsonRpcTransport) -> Any:
        """Start the loading animation."""
        return transport.call("StartLoadingBar", [])

    @_host_method
    def stop_loading_bar(transport: JsonRpcTransport) -> Any:
        """Stop the loading animation."""
        return transport.call("StopLoadingBar", [])


__all__ = ["FlowLauncherAPI", "JsonRpcError"]
