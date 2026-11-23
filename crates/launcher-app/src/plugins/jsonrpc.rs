//! The host for Flow Launcher's out-of-process plugins.
//!
//! Flow Launcher lets plugin authors write a program that speaks JSON-RPC
//! 2.0 over stdio. The plugin announces itself with a `plugin.json` next to
//! its executable, and the launcher starts it, calls `initialize` once and
//! `query` on every keystroke.
//!
//! Three languages are supported, and they differ in two ways: how the
//! process is started, and how a message is delimited on the wire.
//!
//! | `language` in `plugin.json` | process | framing |
//! |---|---|---|
//! | `Python_v2` | `python <file>` | one message per line |
//! | `TypeScript_v2`, `JavaScript_v2` | `node <file>` | `Content-Length` header |
//! | `Executable_v2` | the file itself | one message per line |
//!
//! The in-process .NET plugins of the original are not supported — they need
//! a .NET runtime, and this launcher deliberately ships without one.
//!
//! Everything here is modelled on `JsonRPCPluginV2` and `JsonRPCPublicAPI` in
//! the original's `Flow.Launcher.Core.dll`.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use launcher_core::jsonrpc::{
    encode_error, encode_request, encode_response, error_codes, query_to_json, Framing, Message,
    MessageReader, QueryResponse as JsonRpcQueryResponse,
};
use launcher_core::plugin::{PluginContext, PluginError, PluginResult, QueryPlugin};
use launcher_core::query::Query;
use launcher_core::result::Result as LauncherResult;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

/// How long the launcher waits for a plugin to answer.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a query waits for the plugin to finish starting up.
const READY_TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------- metadata

/// The metadata of an external plugin, as read from its `plugin.json`.
#[derive(Debug, Clone, Default)]
pub struct PluginMetadata {
    /// The plugin id. Used for the settings keys and the result attribution.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The author.
    pub author: String,
    /// The version.
    pub version: String,
    /// The language the plugin is written in.
    pub language: String,
    /// A short description.
    pub description: String,
    /// The plugin's home page.
    pub website: String,
    /// The action keywords the plugin answers to.
    pub action_keywords: Vec<String>,
    /// The plugin icon, relative to the plugin directory.
    pub icon_path: Option<String>,
    /// The file that is executed.
    pub execute_file_name: String,
    /// Whether the plugin is switched off.
    pub disabled: bool,
    /// The directory the plugin lives in.
    pub directory: PathBuf,
}

impl PluginMetadata {
    /// Read the metadata of the plugin in `directory`.
    ///
    /// Returns `None` when the directory holds no usable `plugin.json`.
    pub fn read(directory: &Path) -> Option<Self> {
        let path = directory.join("plugin.json");
        let text = std::fs::read_to_string(&path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        Self::from_json(&value, directory)
    }

    /// Read the metadata from an already parsed `plugin.json`.
    pub fn from_json(value: &Value, directory: &Path) -> Option<Self> {
        let object = value.as_object()?;
        let id = string_field(object, &["ID", "Id", "id"]);
        let execute_file_name = string_field(object, &["ExecuteFileName", "executeFileName"]);
        if id.is_empty() || execute_file_name.is_empty() {
            return None;
        }

        let language = string_field(object, &["Language", "language"]);
        let mut action_keywords = string_list_field(object, &["ActionKeywords", "actionKeywords"]);
        if action_keywords.is_empty() {
            // Older plugins only have the singular form.
            let keyword = string_field(object, &["ActionKeyword", "actionKeyword"]);
            if !keyword.is_empty() {
                action_keywords.push(keyword);
            }
        }

        Some(Self {
            id,
            name: string_field(object, &["Name", "name"]),
            author: string_field(object, &["Author", "author"]),
            version: string_field(object, &["Version", "version"]),
            language,
            description: string_field(object, &["Description", "description"]),
            website: string_field(object, &["Website", "website"]),
            action_keywords,
            icon_path: object
                .get("IcoPath")
                .or_else(|| object.get("icoPath"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            execute_file_name,
            disabled: object
                .get("Disabled")
                .or_else(|| object.get("disabled"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            directory: directory.to_path_buf(),
        })
    }

    /// The file that is executed.
    pub fn execute_file_path(&self) -> PathBuf {
        self.directory.join(&self.execute_file_name)
    }

    /// The metadata as the plugin sees it in the `initialize` call.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "author": self.author,
            "version": self.version,
            "language": self.language,
            "description": self.description,
            "website": self.website,
            "actionKeyword": self.action_keywords.first().map(String::as_str).unwrap_or(""),
            "actionKeywords": self.action_keywords,
            "icoPath": self.icon_path,
            "executeFileName": self.execute_file_name,
            "pluginDirectory": self.directory.to_string_lossy(),
            "disabled": self.disabled,
        })
    }
}

fn field<'a>(object: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return Some(value);
        }
    }
    object
        .iter()
        .find(|(key, _)| names.iter().any(|name| key.eq_ignore_ascii_case(name)))
        .map(|(_, value)| value)
}

fn string_field(object: &Map<String, Value>, names: &[&str]) -> String {
    field(object, names)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn string_list_field(object: &Map<String, Value>, names: &[&str]) -> Vec<String> {
    match field(object, names) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(single)) => vec![single.clone()],
        _ => Vec::new(),
    }
}

// --------------------------------------------------------------- host api

/// The services a plugin may ask the launcher for.
///
/// The original exposes the whole `IPublicAPI` surface to the plugin. This
/// is the part of it that a plugin realistically uses.
pub trait HostApi: Send + Sync {
    /// Show a message to the user.
    fn show_msg(&self, _title: &str, _subtitle: &str) {}
    /// Show an error to the user.
    fn show_error(&self, _title: &str, _subtitle: &str) {}
    /// Put a text on the clipboard.
    fn copy_to_clipboard(&self, _text: &str) {}
    /// Run a command.
    fn shell_run(&self, _command: &str, _filename: &str) {}
    /// Open a URL in the browser.
    fn open_url(&self, _url: &str) {}
    /// Open a directory in the file manager.
    fn open_directory(&self, _path: &str) {}
    /// Put a new query into the search box.
    fn change_query(&self, _query: &str) {}
    /// Hide the launcher window.
    fn hide_window(&self) {}
    /// Show the launcher window.
    fn show_window(&self) {}
    /// Write a line into the log file.
    fn log(&self, _level: &str, _class: &str, _message: &str) {}
    /// Fuzzy-match a query against a text and report the score.
    fn fuzzy_search(&self, _query: &str, _text: &str) -> Option<i32> {
        None
    }
}

/// A host API that does nothing.
///
/// Used when the launcher runs without a window, for instance in a test.
#[derive(Debug, Default)]
pub struct NoopHostApi;

impl HostApi for NoopHostApi {}

// ----------------------------------------------------------------- plugin

/// The state a running plugin shares between the launcher and its own
/// threads.
struct Inner {
    metadata: PluginMetadata,
    framing: Framing,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Mutex<HashMap<i64, Sender<Value>>>,
    next_id: AtomicI64,
    /// The process is running.
    alive: AtomicBool,
    /// The process is on its way up.
    starting: AtomicBool,
    /// Why the plugin failed to start, when it did.
    start_error: Mutex<Option<String>>,
    /// The process answered `initialize`.
    ready: AtomicBool,
    api: Arc<dyn HostApi>,
    /// Where results the plugin pushes on its own are delivered.
    sink: Mutex<Option<Arc<dyn Fn(&str, Vec<LauncherResult>) + Send + Sync>>>,
}

/// An external plugin, running in its own process.
pub struct JsonRpcPlugin {
    inner: Arc<Inner>,
}

impl JsonRpcPlugin {
    /// Create a plugin for the given metadata.
    ///
    /// The process is not started yet; [`QueryPlugin::init`] does that, on a
    /// background thread.
    pub fn new(metadata: PluginMetadata, api: Arc<dyn HostApi>) -> Self {
        let framing = Framing::for_language(&metadata.language).unwrap_or(Framing::NewLineDelimited);
        Self {
            inner: Arc::new(Inner {
                metadata,
                framing,
                stdin: Mutex::new(None),
                child: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicI64::new(1),
                alive: AtomicBool::new(false),
                starting: AtomicBool::new(false),
                start_error: Mutex::new(None),
                ready: AtomicBool::new(false),
                api,
                sink: Mutex::new(None),
            }),
        }
    }

    /// The plugin's metadata.
    pub fn metadata(&self) -> &PluginMetadata {
        &self.inner.metadata
    }

    /// Install the callback that receives results the plugin pushes on its own.
    pub fn set_sink(&self, sink: Arc<dyn Fn(&str, Vec<LauncherResult>) + Send + Sync>) {
        *self.inner.sink.lock() = Some(sink);
    }

    /// Whether the plugin process is running.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// Whether the plugin finished its start-up.
    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
    }

    /// Why the plugin failed to start, when it did.
    pub fn start_error(&self) -> Option<String> {
        self.inner.start_error.lock().clone()
    }

    /// Stop the plugin process.
    pub fn stop(&self) {
        if self.is_ready() {
            let _ = self.send_request("close", vec![], Duration::from_millis(500));
        }
        *self.inner.stdin.lock() = None;
        if let Some(mut child) = self.inner.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.inner.alive.store(false, Ordering::SeqCst);
        self.inner.ready.store(false, Ordering::SeqCst);
    }

    /// Start the plugin process and call `initialize` on it.
    fn start(inner: &Arc<Inner>) -> PluginResult<()> {
        if inner.alive.load(Ordering::SeqCst) {
            return Ok(());
        }
        let metadata = &inner.metadata;
        let Some(framing) = Framing::for_language(&metadata.language) else {
            return Err(PluginError::Init(format!(
                "plugins written in {} are not supported",
                metadata.language
            )));
        };

        let execute_file = metadata.execute_file_path();
        if !execute_file.exists() {
            return Err(PluginError::Init(format!(
                "the plugin file {} does not exist",
                execute_file.display()
            )));
        }

        let mut command = build_command(&metadata, &execute_file)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&metadata.directory);
        hide_console(&mut command);

        let mut child = command
            .spawn()
            .map_err(|e| PluginError::Init(format!("could not start the plugin: {e}")))?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stdin = child.stdin.take().expect("stdin was piped");
        let stderr = child.stderr.take();

        *inner.stdin.lock() = Some(stdin);
        inner.alive.store(true, Ordering::SeqCst);

        // The plugin's own log output goes into our log file.
        if let Some(stderr) = stderr {
            let name = metadata.id.clone();
            std::thread::Builder::new()
                .name(format!("plugin-{name}"))
                .spawn(move || {
                    use std::io::BufRead;
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        log::info!("plugin {name}: {line}");
                    }
                })
                .ok();
        }

        // The reader thread owns the answering of requests.
        let reader_inner = inner.clone();
        std::thread::Builder::new()
            .name(format!("plugin-{}", metadata.id))
            .spawn(move || {
                let mut reader = MessageReader::new(BufReader::new(stdout), framing);
                loop {
                    match reader.read_message() {
                        Ok(Some(Message::Response { id, result, error })) => {
                            let sender = reader_inner.pending.lock().remove(&id);
                            if let Some(sender) = sender {
                                let value = match error {
                                    Some(error) => json!({
                                        "error": error
                                            .get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("the plugin reported an error")
                                    }),
                                    None => result.unwrap_or(Value::Null),
                                };
                                let _ = sender.send(value);
                            }
                        }
                        Ok(Some(Message::Call { id, method, params })) => {
                            handle_call(&reader_inner, id, &method, params);
                        }
                        Ok(None) => break,
                        Err(e) => {
                            log::warn!("the plugin stopped talking: {e}");
                            break;
                        }
                    }
                }
                reader_inner.alive.store(false, Ordering::SeqCst);
                reader_inner.ready.store(false, Ordering::SeqCst);
                reader_inner.pending.lock().clear();
            })
            .ok();

        *inner.child.lock() = Some(child);

        // Tell the plugin who it is talking to.
        let context = json!({
            "currentPluginMetadata": metadata.to_json(),
            "api": {},
        });
        // The request is sent through the shared state, not through a
        // temporary plugin object: a temporary would be dropped at the end
        // of this function, and dropping a plugin stops it.
        send_request(inner, "initialize", vec![context], DEFAULT_TIMEOUT)?;
        inner.ready.store(true, Ordering::SeqCst);
        log::info!(
            "plugin {} ({}) started",
            metadata.id,
            metadata.language
        );
        Ok(())
    }

    /// Send a request and wait for the answer.
    fn send_request(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, PluginError> {
        send_request(&self.inner, method, params, timeout)
    }

    /// Wait until the plugin has finished its start-up.
    fn wait_until_ready(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.is_ready() {
                return true;
            }
            if !self.is_alive() && !self.inner.starting.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

/// Send a request to the plugin and wait for the answer.
///
/// This works on the shared state rather than on a plugin object, so that it
/// can be used while the plugin is still starting up.
fn send_request(
    inner: &Arc<Inner>,
    method: &str,
    params: Vec<Value>,
    timeout: Duration,
) -> Result<Value, PluginError> {
    if !inner.alive.load(Ordering::SeqCst) {
        return Err(PluginError::Query("the plugin is not running".into()));
    }
    let id = inner.next_id.fetch_add(1, Ordering::SeqCst);
    let (sender, receiver): (Sender<Value>, Receiver<Value>) = channel();
    inner.pending.lock().insert(id, sender);

    let bytes = encode_request(inner.framing, id, method, params);
    if let Err(e) = write(inner, &bytes) {
        inner.pending.lock().remove(&id);
        return Err(PluginError::Query(format!(
            "could not talk to the plugin: {e}"
        )));
    }

    match receiver.recv_timeout(timeout) {
        Ok(value) => {
            if let Some(message) = value.get("error").and_then(Value::as_str) {
                return Err(PluginError::Query(message.to_owned()));
            }
            Ok(value)
        }
        Err(_) => {
            inner.pending.lock().remove(&id);
            Err(PluginError::Query(format!(
                "the plugin did not answer {method} within {timeout:?}"
            )))
        }
    }
}

/// Write a message to the plugin.
fn write(inner: &Arc<Inner>, bytes: &[u8]) -> std::io::Result<()> {
    let mut guard = inner.stdin.lock();
    let Some(stdin) = guard.as_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "the plugin is not running",
        ));
    };
    stdin.write_all(bytes)?;
    stdin.flush()
}

impl Drop for JsonRpcPlugin {
    fn drop(&mut self) {
        self.stop();
    }
}

impl QueryPlugin for JsonRpcPlugin {
    fn id(&self) -> &str {
        &self.inner.metadata.id
    }

    fn name(&self) -> &str {
        &self.inner.metadata.name
    }

    fn action_keywords(&self) -> &[String] {
        &self.inner.metadata.action_keywords
    }

    fn init(&self, _ctx: &PluginContext) -> PluginResult<()> {
        // Starting a plugin means starting a process, and a Python plugin
        // takes a while to come up. That must not hold up the launcher's own
        // start-up, so the process is started on a background thread.
        let inner = self.inner.clone();
        inner.starting.store(true, Ordering::SeqCst);
        std::thread::Builder::new()
            .name(format!("plugin-start-{}", inner.metadata.id))
            .spawn(move || {
                let outcome = JsonRpcPlugin::start(&inner);
                inner.starting.store(false, Ordering::SeqCst);
                if let Err(e) = outcome {
                    let message = e.to_string();
                    log::warn!("plugin {} did not start: {message}", inner.metadata.id);
                    *inner.start_error.lock() = Some(message);
                }
            })
            .ok();
        Ok(())
    }

    fn query(&self, query: &Query) -> PluginResult<Vec<LauncherResult>> {
        // The plugin may still be starting up; give it a moment.
        self.wait_until_ready(READY_TIMEOUT);
        let params = vec![query_to_json(query), json!({})];
        let value = self.send_request("query", params, DEFAULT_TIMEOUT)?;
        let mut response =
            JsonRpcQueryResponse::from_json(&value, Some(&self.inner.metadata.directory));
        // The plugin does not have to name itself; the original fills the
        // plugin id in after the query comes back.
        for result in &mut response.results {
            if result.plugin_id.is_empty() {
                result.plugin_id = self.inner.metadata.id.clone();
            }
        }
        if !response.debug_message.is_empty() {
            log::info!("plugin {}: {}", self.inner.metadata.id, response.debug_message);
        }
        Ok(response.results)
    }

    fn execute(&self, result: &LauncherResult) -> PluginResult<bool> {
        let Some(action) = &result.action else {
            return Ok(false);
        };
        let method = action.get("method").and_then(Value::as_str).unwrap_or("");
        if method.is_empty() {
            return Ok(true);
        }
        let parameters = action
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let dont_hide = action
            .get("dontHideAfterAction")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        self.send_request(method, parameters, DEFAULT_TIMEOUT)?;
        Ok(!dont_hide)
    }

    fn reload(&self) -> PluginResult<()> {
        if !self.is_ready() {
            return Ok(());
        }
        let context = json!({
            "currentPluginMetadata": self.inner.metadata.to_json(),
            "api": {},
        });
        match self.send_request("reload_data", vec![context], DEFAULT_TIMEOUT) {
            Ok(_) => Ok(()),
            // The original ignores a plugin that does not implement it.
            Err(PluginError::Query(message)) => {
                log::info!(
                    "plugin {} does not reload: {message}",
                    self.inner.metadata.id
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Handle a call the plugin made to the host.
fn handle_call(inner: &Arc<Inner>, id: Option<i64>, method: &str, params: Vec<Value>) {
    let api = inner.api.clone();

    // The one method the original adds itself: the plugin pushes results
    // before it answers the query.
    if method.eq_ignore_ascii_case("UpdateResults") {
        let query = params.first().and_then(Value::as_str).unwrap_or("");
        let response = params.get(1).cloned().unwrap_or(Value::Null);
        let parsed =
            JsonRpcQueryResponse::from_json(&response, Some(&inner.metadata.directory));
        if let Some(sink) = inner.sink.lock().as_ref() {
            sink(query, parsed.results);
        }
        return;
    }

    let argument = |index: usize| -> String {
        params
            .get(index)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    let result = match method {
        "ShowMsg" => {
            api.show_msg(&argument(0), &argument(1));
            Value::Null
        }
        "ShowMsgError" => {
            api.show_error(&argument(0), &argument(1));
            Value::Null
        }
        "CopyToClipboard" => {
            api.copy_to_clipboard(&argument(0));
            Value::Null
        }
        "ShellRun" => {
            api.shell_run(&argument(0), &argument(1));
            Value::Null
        }
        "OpenUrl" => {
            api.open_url(&argument(0));
            Value::Null
        }
        "OpenDirectory" => {
            api.open_directory(&argument(0));
            Value::Null
        }
        "OpenAppUri" => {
            api.open_url(&argument(0));
            Value::Null
        }
        "ChangeQuery" => {
            api.change_query(&argument(0));
            Value::Null
        }
        "HideMainWindow" => {
            api.hide_window();
            Value::Null
        }
        "ShowMainWindow" => {
            api.show_window();
            Value::Null
        }
        "LogDebug" | "LogInfo" | "LogWarn" | "LogError" => {
            let level = method.trim_start_matches("Log").to_ascii_lowercase();
            api.log(&level, &argument(0), &argument(1));
            Value::Null
        }
        "FuzzySearch" => match api.fuzzy_search(&argument(0), &argument(1)) {
            Some(score) => json!({ "score": score, "matchData": [] }),
            None => json!({ "score": 0, "matchData": [] }),
        },
        "IsMainWindowVisible" => Value::Bool(true),
        "GetTranslation" => Value::String(argument(0)),
        "StartLoadingBar" | "StopLoadingBar" | "SavePluginCaches" | "SaveAppAllSettings"
        | "SavePluginSettings" | "BackToQueryResults" | "CheckForNewUpdate"
        | "OpenSettingDialog" | "RestartApp" | "ReloadAllPluginDataAsync" => Value::Null,
        _ => {
            // The original answers with method-not-found for anything it does
            // not know.
            if let Some(id) = id {
                let bytes = encode_error(
                    inner.framing,
                    id,
                    error_codes::METHOD_NOT_FOUND,
                    &format!("the host does not implement {method}"),
                );
                let mut guard = inner.stdin.lock();
                if let Some(stdin) = guard.as_mut() {
                    let _ = stdin.write_all(&bytes);
                    let _ = stdin.flush();
                }
            }
            return;
        }
    };

    if let Some(id) = id {
        let bytes = encode_response(inner.framing, id, result);
        let mut guard = inner.stdin.lock();
        if let Some(stdin) = guard.as_mut() {
            let _ = stdin.write_all(&bytes);
            let _ = stdin.flush();
        }
    }
}

/// The `CREATE_NO_WINDOW` flag, so a console plugin does not flash a window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep a console plugin from flashing a window while it starts.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// Build the command that starts the plugin.
fn build_command(metadata: &PluginMetadata, execute_file: &Path) -> PluginResult<Command> {
    let language = metadata.language.trim().to_ascii_lowercase();
    let mut command = match language.as_str() {
        "python" | "python_v2" | "pythonv2" => {
            let mut command = python_command();
            command.arg(execute_file);
            command
        }
        "typescript" | "typescript_v2" | "typescriptv2" | "javascript" | "javascript_v2"
        | "javascriptv2" | "node" | "node_v2" => {
            let mut command = Command::new("node");
            command.arg(execute_file);
            command
        }
        "executable" | "executable_v2" | "executablev2" => Command::new(execute_file),
        other => {
            return Err(PluginError::Init(format!(
                "plugins written in {other} are not supported"
            )));
        }
    };

    // The original hands these to the plugin; some of them use them.
    command.env("FLOW_VERSION", env!("CARGO_PKG_VERSION"));
    // The plugin must not write its byte code into the installation
    // directory — that directory may well be read-only, and the leftovers
    // would survive an uninstall.
    command.env("PYTHONDONTWRITEBYTECODE", "1");
    if let Ok(exe) = std::env::current_exe() {
        if let Some(directory) = exe.parent() {
            command.env("FLOW_PROGRAM_DIRECTORY", directory);
            command.env("FLOW_APPLICATION_DIRECTORY", directory);
        }
    }

    // The runtime helpers the plugin may import.
    if let Some(runtime) = runtime_directory() {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let python = runtime.join("python");
        if python.is_dir() {
            let existing = std::env::var("PYTHONPATH").unwrap_or_default();
            let path = if existing.is_empty() {
                python.to_string_lossy().to_string()
            } else {
                format!("{}{separator}{existing}", python.to_string_lossy())
            };
            command.env("PYTHONPATH", path);
        }
        let node = runtime.join("node");
        if node.is_dir() {
            let existing = std::env::var("NODE_PATH").unwrap_or_default();
            let path = if existing.is_empty() {
                node.to_string_lossy().to_string()
            } else {
                format!("{}{separator}{existing}", node.to_string_lossy())
            };
            command.env("NODE_PATH", path);
        }
    }

    Ok(command)
}

/// Find a Python interpreter to run a plugin with.
///
/// The Microsoft Store ships a `python.exe` stub that only prints an error
/// and exits with 9009, so a candidate has to be *run*, not just found — the
/// stub is usually the first `python.exe` on the `PATH`.
fn python_command() -> Command {
    static PYTHON: std::sync::OnceLock<Option<(String, Vec<String>)>> = std::sync::OnceLock::new();

    let chosen = PYTHON.get_or_init(|| {
        for (program, args) in python_candidates() {
            if python_works(&program, &args) {
                return Some((program, args));
            }
        }
        None
    });

    match chosen {
        Some((program, args)) => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
        None => Command::new("python"),
    }
}

/// The interpreters to try, in order of preference.
fn python_candidates() -> Vec<(String, Vec<String>)> {
    let mut candidates: Vec<(String, Vec<String>)> = vec![
        // `py -3` is the launcher that comes with every Python install.
        ("py".to_string(), vec!["-3".to_string()]),
        ("python".to_string(), Vec::new()),
    ];

    // The per-user installs the official installer leaves behind, newest
    // first.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("Programs").join("Python");
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.join("python.exe").exists())
                .collect();
            versions.sort_by_key(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
            versions.reverse();
            for path in versions {
                candidates.push((
                    path.join("python.exe").to_string_lossy().to_string(),
                    Vec::new(),
                ));
            }
        }
    }

    candidates
}

/// Whether the interpreter really is a Python.
fn python_works(program: &str, args: &[String]) -> bool {
    let mut command = Command::new(program);
    hide_console(&mut command);
    let Ok(output) = command.args(args).arg("--version").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.to_string()
    } else {
        stdout.to_string()
    };
    text.to_ascii_lowercase().contains("python")
}

/// The directory that holds the runtime helpers for the plugins.
///
/// It sits next to the executable, so the helpers travel with the
/// installation.
pub fn runtime_directory() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(directory) = exe.parent() {
            candidates.push(directory.join("runtime"));
        }
    }
    candidates.push(crate::paths::data_directory().join("Runtime"));
    candidates.into_iter().find(|candidate| candidate.is_dir())
}

/// Find the plugins the user installed.
///
/// Each plugin lives in its own directory below `directory` and announces
/// itself with a `plugin.json`.
pub fn discover_plugins(directory: &Path) -> Vec<PluginMetadata> {
    let mut plugins = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return plugins;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match PluginMetadata::read(&path) {
            Some(metadata) => {
                if metadata.disabled {
                    log::info!("plugin {} is switched off", metadata.id);
                    continue;
                }
                plugins.push(metadata);
            }
            None => continue,
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(directory: &Path, json: &str) {
        std::fs::create_dir_all(directory).expect("create");
        std::fs::write(directory.join("plugin.json"), json).expect("write");
    }

    #[test]
    fn reads_the_metadata_of_a_plugin() {
        let directory = std::env::temp_dir().join("ideas-launcher-jsonrpc-test");
        write_plugin(
            &directory,
            r#"{
                "ID": "demo",
                "Name": "Demo",
                "Author": "someone",
                "Version": "1.2.3",
                "Language": "Python_v2",
                "Description": "a demo plugin",
                "ActionKeywords": ["dm"],
                "ExecuteFileName": "main.py",
                "IcoPath": "icon.png"
            }"#,
        );
        let metadata = PluginMetadata::read(&directory).expect("metadata");
        assert_eq!(metadata.id, "demo");
        assert_eq!(metadata.name, "Demo");
        assert_eq!(metadata.version, "1.2.3");
        assert_eq!(metadata.language, "Python_v2");
        assert_eq!(metadata.action_keywords, vec!["dm".to_string()]);
        assert_eq!(metadata.execute_file_name, "main.py");
        assert_eq!(metadata.icon_path.as_deref(), Some("icon.png"));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_old_plugin_with_only_one_keyword_still_works() {
        let directory = std::env::temp_dir().join("ideas-launcher-jsonrpc-old");
        write_plugin(
            &directory,
            r#"{
                "ID": "old",
                "Name": "Old",
                "Language": "Executable_V2",
                "ActionKeyword": "ol",
                "ExecuteFileName": "run.exe"
            }"#,
        );
        let metadata = PluginMetadata::read(&directory).expect("metadata");
        assert_eq!(metadata.action_keywords, vec!["ol".to_string()]);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_plugin_without_an_executable_file_is_rejected() {
        let directory = std::env::temp_dir().join("ideas-launcher-jsonrpc-broken");
        write_plugin(&directory, r#"{"ID": "broken", "Name": "Broken"}"#);
        assert!(PluginMetadata::read(&directory).is_none());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn discovers_the_plugins_of_a_directory() {
        let root = std::env::temp_dir().join("ideas-launcher-jsonrpc-discover");
        let _ = std::fs::remove_dir_all(&root);
        write_plugin(
            &root.join("a"),
            r#"{"ID": "a", "Name": "A", "Language": "Python_v2", "ExecuteFileName": "a.py"}"#,
        );
        write_plugin(
            &root.join("b"),
            r#"{"ID": "b", "Name": "B", "Language": "Python_v2", "ExecuteFileName": "b.py"}"#,
        );
        write_plugin(
            &root.join("off"),
            r#"{"ID": "c", "Name": "C", "Language": "Python_v2", "ExecuteFileName": "c.py", "Disabled": true}"#,
        );
        let found = discover_plugins(&root);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "a");
        assert_eq!(found[1].id, "b");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_metadata_is_sent_in_the_shape_the_plugin_expects() {
        let metadata = PluginMetadata {
            id: "demo".into(),
            name: "Demo".into(),
            language: "Python_v2".into(),
            action_keywords: vec!["dm".into()],
            execute_file_name: "main.py".into(),
            directory: PathBuf::from(r"C:\plugins\demo"),
            ..Default::default()
        };
        let value = metadata.to_json();
        assert_eq!(value["id"], "demo");
        assert_eq!(value["actionKeyword"], "dm");
        assert_eq!(value["actionKeywords"][0], "dm");
        assert_eq!(value["pluginDirectory"], r"C:\plugins\demo");
    }

    #[test]
    fn an_unsupported_language_is_refused() {
        let metadata = PluginMetadata {
            id: "dotnet".into(),
            language: "CSharp".into(),
            execute_file_name: "plugin.dll".into(),
            ..Default::default()
        };
        let command = build_command(&metadata, Path::new("plugin.dll"));
        assert!(command.is_err());
    }
}
