//! The plugin contract.
//!
//! Mirrors the capability interfaces in Flow Launcher's
//! `Flow.Launcher.Plugin.dll` — `IAsyncPlugin`, `IAsyncReloadable`,
//! `IAsyncHomeQuery`, `IContextMenu`, `IReloadable`, `ISavable` — as a single
//! trait plus optional extension traits.
//!
//! Plugins are invoked synchronously. In-process plugins are CPU bound, and
//! the engine runs them on a thread pool anyway; an out-of-process plugin
//! bridges over its IPC channel and blocks on the reply, exactly as the
//! original implementation does with its `StreamJsonRpc` client.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::query::Query;
use crate::result::Result as LauncherResult;

/// Errors raised by a plugin.
#[derive(Debug)]
pub enum PluginError {
    /// The plugin failed during initialisation.
    Init(String),
    /// The plugin failed while answering a query.
    Query(String),
    /// The plugin is not able to serve the request.
    Unsupported(String),
    /// Anything else.
    Other(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Init(m) => write!(f, "plugin init failed: {m}"),
            PluginError::Query(m) => write!(f, "plugin query failed: {m}"),
            PluginError::Unsupported(m) => write!(f, "unsupported: {m}"),
            PluginError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl Error for PluginError {}

impl From<String> for PluginError {
    fn from(value: String) -> Self {
        PluginError::Other(value)
    }
}

impl From<&str> for PluginError {
    fn from(value: &str) -> Self {
        PluginError::Other(value.to_string())
    }
}

/// Result alias used throughout the plugin API.
pub type PluginResult<T> = Result<T, PluginError>;

/// Services a plugin may ask the host for.
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// Directory where the plugin may store settings.
    pub settings_directory: PathBuf,
    /// Directory where the plugin may store cached data.
    pub cache_directory: PathBuf,
    /// The plugin's own id.
    pub plugin_id: String,
}

impl PluginContext {
    pub fn new(
        plugin_id: impl Into<String>,
        settings_directory: impl Into<PathBuf>,
        cache_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            settings_directory: settings_directory.into(),
            cache_directory: cache_directory.into(),
        }
    }
}

/// A plugin that can answer queries.
pub trait QueryPlugin: Send + Sync {
    /// Stable identifier, used for settings keys and result attribution.
    fn id(&self) -> &str;

    /// Human readable name.
    fn name(&self) -> &str;

    /// Keywords that route a query to this plugin.
    ///
    /// The wildcard `*` means "always included".
    fn action_keywords(&self) -> &[String] {
        &[]
    }

    /// Called once when the plugin is registered.
    fn init(&self, _ctx: &PluginContext) -> PluginResult<()> {
        Ok(())
    }

    /// Answer a query.
    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>>;

    /// Run the action of a result the user selected.
    ///
    /// Most plugins are inert here — the launcher opens the file or runs the
    /// command itself. A plugin that wants to do the work itself (an
    /// out-of-process one, typically) puts an action into the result and
    /// overrides this.
    ///
    /// Returns whether the window should close afterwards.
    fn execute(&self, _result: &LauncherResult) -> PluginResult<bool> {
        Ok(false)
    }

    /// Called when the user asks for all plugin data to be reloaded.
    fn reload(&self) -> PluginResult<()> {
        Ok(())
    }
}

/// Optional extension: a plugin that contributes results to the home page.
pub trait HomeQueryPlugin: Send + Sync {
    fn home_query(&self, _query: &Query) -> PluginResult<Vec<LauncherResult>> {
        Ok(Vec::new())
    }
}

/// Optional extension: a plugin that provides its own settings.
pub trait SettingsProvider: Send + Sync {
    /// Returns a serialisable settings object.
    fn settings(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Applies a settings object.
    fn apply_settings(&mut self, _value: &serde_json::Value) -> PluginResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl QueryPlugin for Echo {
        fn id(&self) -> &str {
            "echo"
        }
        fn name(&self) -> &str {
            "Echo"
        }
        fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
            Ok(vec![LauncherResult::new(query.search.clone())])
        }
    }

    #[test]
    fn plugin_is_usable_as_a_trait_object() {
        let plugin: Box<dyn QueryPlugin> = Box::new(Echo);
        assert_eq!(plugin.id(), "echo");
    }

    #[test]
    fn plugin_error_is_displayable() {
        let e = PluginError::Query("boom".into());
        assert!(e.to_string().contains("boom"));
    }
}
