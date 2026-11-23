//! The JSON-RPC 2.0 wire protocol used by Flow Launcher's external plugins.
//!
//! Flow Launcher talks to out-of-process plugins over stdio using JSON-RPC
//! 2.0. Two framings are in use, picked by the plugin's language:
//!
//! | plugin language | framing | reference |
//! |---|---|---|
//! | `Python_v2`, `Executable_V2` | one JSON message per line | `PythonPluginV2`, `ExecutablePluginV2` |
//! | `TypeScript_v2`, `JavaScript_v2` | `Content-Length` header, like LSP | `NodePluginV2` |
//!
//! The host calls five methods on the plugin:
//!
//! * `initialize(context)` — once, right after the process starts
//! * `query(query, settings)` — for every keystroke
//! * `context_menu(contextData)` — when the user asks for a context menu
//! * `reload_data(context)` — when the user asks every plugin to reload
//! * `close()` — when the plugin is shut down
//!
//! The plugin may call one method back on the host, `UpdateResults(query,
//! response)`, to push results before the query itself is answered.
//!
//! Everything in this module is deliberately free of I/O ownership: the codec
//! works on byte slices and on any [`BufRead`], so it can be tested without
//! spawning a process.

use std::io::{self, BufRead};

use serde_json::{json, Value};

use crate::query::Query;
use crate::result::{Result as LauncherResult, ResultIcon};

/// How a JSON-RPC message is delimited on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// One JSON message per line, terminated by `\n`.
    ///
    /// Used by the Python and executable plugins.
    NewLineDelimited,
    /// A `Content-Length` header followed by the payload.
    ///
    /// Used by the Node.js plugins; identical to the framing the Language
    /// Server Protocol uses.
    HeaderDelimited,
}

impl Framing {
    /// The framing a plugin of the given language uses.
    ///
    /// Returns `None` for languages that are not driven over JSON-RPC — the
    /// in-process .NET plugins, for instance.
    pub fn for_language(language: &str) -> Option<Self> {
        match language.trim().to_ascii_lowercase().as_str() {
            "python" | "python_v2" | "pythonv2" => Some(Framing::NewLineDelimited),
            "executable" | "executable_v2" | "executablev2" => Some(Framing::NewLineDelimited),
            "typescript" | "typescript_v2" | "typescriptv2" | "javascript"
            | "javascript_v2" | "javascriptv2" | "node" | "node_v2" => {
                Some(Framing::HeaderDelimited)
            }
            _ => None,
        }
    }
}

/// Encode a JSON-RPC request.
pub fn encode_request(framing: Framing, id: i64, method: &str, params: Vec<Value>) -> Vec<u8> {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    frame(framing, &message)
}

/// Encode a JSON-RPC notification — a request without an id, so no answer is
/// expected.
pub fn encode_notification(framing: Framing, method: &str, params: Vec<Value>) -> Vec<u8> {
    let message = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    frame(framing, &message)
}

/// Encode a successful JSON-RPC response.
pub fn encode_response(framing: Framing, id: i64, result: Value) -> Vec<u8> {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    frame(framing, &message)
}

/// Encode a JSON-RPC error response.
pub fn encode_error(framing: Framing, id: i64, code: i64, message: &str) -> Vec<u8> {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    frame(framing, &message)
}

fn frame(framing: Framing, message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_else(|_| b"{}".to_vec());
    match framing {
        Framing::NewLineDelimited => {
            let mut out = body;
            out.push(b'\n');
            out
        }
        Framing::HeaderDelimited => {
            let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
            out.extend_from_slice(&body);
            out
        }
    }
}

/// The JSON-RPC error codes the host uses.
pub mod error_codes {
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The parameters do not fit the method.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Something went wrong inside the host.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A message read off the wire.
#[derive(Debug, Clone)]
pub enum Message {
    /// A response to a request the host sent.
    Response {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// A request or notification the plugin sent.
    Call { id: Option<i64>, method: String, params: Vec<Value> },
}

impl Message {
    /// Parse a decoded JSON value into a message.
    pub fn from_value(value: Value) -> Option<Self> {
        let object = value.as_object()?;
        let method = object.get("method").and_then(Value::as_str).map(str::to_owned);
        match method {
            Some(method) => {
                let id = object.get("id").and_then(Value::as_i64);
                let params = match object.get("params") {
                    Some(Value::Array(items)) => items.clone(),
                    Some(Value::Null) | None => Vec::new(),
                    Some(other) => vec![other.clone()],
                };
                Some(Message::Call { id, method, params })
            }
            None => {
                let id = object.get("id").and_then(Value::as_i64)?;
                let result = object.get("result").cloned();
                let error = object.get("error").cloned();
                Some(Message::Response { id, result, error })
            }
        }
    }
}

/// Reads framed JSON-RPC messages from a stream.
pub struct MessageReader<R: BufRead> {
    inner: R,
    framing: Framing,
}

impl<R: BufRead> MessageReader<R> {
    pub fn new(inner: R, framing: Framing) -> Self {
        Self { inner, framing }
    }

    /// Read the next message.
    ///
    /// Returns `Ok(None)` at the end of the stream.
    pub fn read_message(&mut self) -> io::Result<Option<Message>> {
        loop {
            let raw = match self.framing {
                Framing::NewLineDelimited => self.read_line()?,
                Framing::HeaderDelimited => self.read_header_delimited()?,
            };
            let Some(raw) = raw else {
                return Ok(None);
            };
            if raw.iter().all(|b| b.is_ascii_whitespace()) {
                // A blank line is a keep-alive in the header framing; in the
                // line framing it is simply an empty message.
                continue;
            }
            match serde_json::from_slice::<Value>(&raw) {
                Ok(value) => {
                    if let Some(message) = Message::from_value(value) {
                        return Ok(Some(message));
                    }
                }
                Err(e) => {
                    log::warn!("a plugin sent a message that is not valid JSON: {e}");
                }
            }
        }
    }

    fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        let read = self.inner.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        // Strip the line terminator.
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line.pop();
        }
        Ok(Some(line))
    }

    fn read_header_delimited(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let read = self.inner.read_line(&mut line)?;
            if read == 0 {
                // The stream ended in the middle of a header block.
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().ok();
                }
            }
        }

        let Some(length) = length else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a plugin sent a header block without a Content-Length",
            ));
        };

        let mut body = vec![0u8; length];
        self.inner.read_exact(&mut body)?;
        Ok(Some(body))
    }
}

// ------------------------------------------------------------------ mapping

/// The `Query` object as the plugin sees it.
pub fn query_to_json(query: &Query) -> Value {
    json!({
        "originalQuery": query.original_query,
        "trimmedQuery": query.trimmed_query,
        "isReQuery": query.is_requery,
        "isHomeQuery": query.is_home_query,
        "search": query.search,
        "searchTerms": query.search_terms,
        "actionKeyword": query.action_keyword,
    })
}

/// The `Result` object as the plugin sees it.
pub fn result_to_json(result: &LauncherResult) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("title".into(), json!(result.title));
    object.insert("subTitle".into(), json!(result.subtitle));
    object.insert("titleToolTip".into(), json!(result.title_tooltip));
    object.insert("subTitleToolTip".into(), json!(result.subtitle_tooltip));
    object.insert("score".into(), json!(result.score));
    object.insert("autoCompleteText".into(), json!(result.auto_complete_text));
    object.insert("copyText".into(), json!(result.copy_text));
    object.insert("pluginID".into(), json!(result.plugin_id));
    object.insert("actionKeywordAssigned".into(), json!(result.plugin_id));
    object.insert("recordKey".into(), json!(result.record_key));

    match &result.icon {
        ResultIcon::Path(path) => {
            object.insert("icoPath".into(), json!(path.to_string_lossy()));
        }
        ResultIcon::Glyph {
            font_family,
            glyph,
        } => {
            object.insert(
                "glyph".into(),
                json!({ "fontFamily": font_family, "glyph": glyph }),
            );
        }
        _ => {}
    }

    if let Some(context) = &result.context_data {
        // The plugin may hand us arbitrary JSON; keep it as JSON when it
        // parses, and as a string otherwise.
        let value = serde_json::from_str::<Value>(context)
            .unwrap_or_else(|_| Value::String(context.clone()));
        object.insert("contextData".into(), value);
    }

    if let Some(preview) = &result.preview {
        object.insert(
            "preview".into(),
            json!({
                "description": preview.description,
                "previewImagePath": preview.image_path,
                "filePath": preview.file_path,
                "contentType": preview.content_type.as_str(),
                "isMedia": preview.is_media,
            }),
        );
        object.insert(
            "previewVisibility".into(),
            json!(preview.visibility.as_str()),
        );
    }

    if !result.title_highlight.is_empty() {
        // Flow Launcher expects a flat list of character indices.
        let mut indices = Vec::with_capacity(result.title_highlight.len() * 2);
        for (start, end) in &result.title_highlight {
            indices.push(*start);
            indices.push(end.saturating_sub(*start));
        }
        object.insert("titleHighlightData".into(), json!(indices));
    }

    if let Some(progress) = result.progress {
        object.insert("progressBar".into(), json!(progress));
    }

    Value::Object(object)
}

/// Read a `Result` object as the plugin sent it.
///
/// Unknown fields are ignored and missing fields fall back to their defaults,
/// which mirrors how the original deserialises the payload.
pub fn result_from_json(value: &Value, plugin_directory: Option<&std::path::Path>) -> LauncherResult {
    let object = match value.as_object() {
        Some(object) => object,
        None => return LauncherResult::default(),
    };

    let title = string_field(object, "title");
    let subtitle = string_field(object, "subTitle");
    let icon = icon_from_json(object, plugin_directory);
    let context_data = object
        .get("contextData")
        .filter(|v| !v.is_null())
        .map(|v| v.to_string());

    // The action the plugin wants to run when the row is selected.
    let action = object
        .get("jsonRPCAction")
        .or_else(|| object.get("JsonRPCAction"))
        .filter(|v| !v.is_null())
        .cloned();

    let mut result = LauncherResult {
        title,
        subtitle,
        title_tooltip: string_field(object, "titleToolTip"),
        subtitle_tooltip: string_field(object, "subTitleToolTip"),
        icon,
        badge_icon: ResultIcon::None,
        show_badge: object
            .get("showBadge")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        score: object.get("score").and_then(Value::as_i64).unwrap_or(0) as i32,
        auto_complete_text: string_field(object, "autoCompleteText"),
        copy_text: string_field(object, "copyText"),
        context_data,
        action,
        plugin_id: string_field(object, "pluginID"),
        title_highlight: highlight_from_json(object.get("titleHighlightData")),
        record_key: object
            .get("recordKey")
            .and_then(Value::as_str)
            .map(str::to_owned),
        preview: preview_from_json(object.get("preview")),
        progress: object
            .get("progressBar")
            .and_then(Value::as_i64)
            .map(|v| v.clamp(0, 100) as u8),
        // A plugin may say how big the file is and when it changed. It is
        // free information, so it is taken when it is there.
        size: object.get("size").and_then(Value::as_u64),
        modified: object.get("modified").and_then(Value::as_u64),
        // An external plugin matched whatever it matched — its rows sit in
        // the middle group either way, so the flag is simply at its default.
        path_only_match: false,
    };

    if result.plugin_id.is_empty() {
        result.plugin_id = string_field(object, "pluginId");
    }

    result
}

fn string_field(object: &serde_json::Map<String, Value>, name: &str) -> String {
    object
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn icon_from_json(
    object: &serde_json::Map<String, Value>,
    plugin_directory: Option<&std::path::Path>,
) -> ResultIcon {
    if let Some(glyph) = object.get("glyph").and_then(Value::as_object) {
        let font_family = glyph
            .get("fontFamily")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let character = glyph
            .get("glyph")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !character.is_empty() {
            return ResultIcon::Glyph {
                font_family,
                glyph: character,
            };
        }
    }

    let ico_path = string_field(object, "icoPath");
    if ico_path.is_empty() {
        return ResultIcon::None;
    }

    // A relative path is relative to the plugin's own directory.
    let path = std::path::PathBuf::from(&ico_path);
    let path = if path.is_absolute() {
        path
    } else if let Some(directory) = plugin_directory {
        directory.join(path)
    } else {
        path
    };
    ResultIcon::Path(path)
}

/// Flow Launcher sends the highlighted character indices as a flat list of
/// `[start, length, start, length, …]` pairs.
fn highlight_from_json(value: Option<&Value>) -> Vec<(usize, usize)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    if items.len() % 2 != 0 {
        return Vec::new();
    }
    let mut ranges = Vec::with_capacity(items.len() / 2);
    for pair in items.chunks(2) {
        let Some(start) = pair[0].as_u64() else {
            return Vec::new();
        };
        let Some(length) = pair[1].as_u64() else {
            return Vec::new();
        };
        let start = start as usize;
        let end = start + length as usize;
        ranges.push((start, end));
    }
    ranges
}

fn preview_from_json(value: Option<&Value>) -> Option<crate::result::PreviewData> {
    let object = value?.as_object()?;
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let image_path = object
        .get("previewImagePath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let file_path = object
        .get("filePath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let content_type = match object.get("contentType").and_then(Value::as_str) {
        Some("markdown") => crate::result::PreviewContentType::Markdown,
        _ => crate::result::PreviewContentType::ImageWithText,
    };
    let is_media = object
        .get("isMedia")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if description.is_empty() && image_path.is_none() && file_path.is_none() {
        return None;
    }
    Some(crate::result::PreviewData {
        description,
        image_path,
        file_path,
        content_type,
        is_media,
        visibility: crate::result::PreviewVisibility::Optional,
    })
}

/// The payload the plugin answers a `query` with.
///
/// The original nests the results inside an envelope that also carries the
/// settings the plugin wants to persist and a free-form debug message.
#[derive(Debug, Clone, Default)]
pub struct QueryResponse {
    pub results: Vec<LauncherResult>,
    pub settings_change: Option<Value>,
    pub debug_message: String,
}

impl QueryResponse {
    /// Read the envelope the plugin sent.
    pub fn from_json(value: &Value, plugin_directory: Option<&std::path::Path>) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let results = match object.get("result") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| result_from_json(item, plugin_directory))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            results,
            settings_change: object.get("settingsChange").cloned().filter(|v| !v.is_null()),
            debug_message: object
                .get("debugMessage")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    /// The envelope as the host would send it back to the plugin.
    pub fn to_json(&self) -> Value {
        json!({
            "id": 0,
            "result": self.results.iter().map(result_to_json).collect::<Vec<_>>(),
            "settingsChange": self.settings_change,
            "debugMessage": self.debug_message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn language_maps_to_a_framing() {
        assert_eq!(
            Framing::for_language("Python_v2"),
            Some(Framing::NewLineDelimited)
        );
        assert_eq!(
            Framing::for_language("TypeScript_V2"),
            Some(Framing::HeaderDelimited)
        );
        assert_eq!(
            Framing::for_language("Executable_v2"),
            Some(Framing::NewLineDelimited)
        );
        assert_eq!(Framing::for_language("CSharp"), None);
    }

    #[test]
    fn newline_framing_writes_one_message_per_line() {
        let bytes = encode_request(
            Framing::NewLineDelimited,
            7,
            "query",
            vec![json!("hello")],
        );
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1);
        let value: Value = serde_json::from_str(text.trim_end()).expect("json");
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "query");
        assert_eq!(value["params"][0], "hello");
    }

    #[test]
    fn header_framing_writes_a_content_length() {
        let bytes = encode_request(Framing::HeaderDelimited, 1, "close", vec![]);
        let text = String::from_utf8(bytes).expect("utf8");
        let (header, body) = text.split_once("\r\n\r\n").expect("header");
        assert!(header.to_ascii_lowercase().contains("content-length"));
        let length: usize = header
            .split(':')
            .nth(1)
            .expect("value")
            .trim()
            .parse()
            .expect("number");
        assert_eq!(length, body.len());
    }

    #[test]
    fn reads_newline_delimited_messages() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n";
        let mut reader = MessageReader::new(Cursor::new(input), Framing::NewLineDelimited);
        let first = reader.read_message().expect("read").expect("message");
        match first {
            Message::Response { id, result, error } => {
                assert_eq!(id, 1);
                assert_eq!(result.expect("result")["a"], 1);
                assert!(error.is_none());
            }
            other => panic!("unexpected message: {other:?}"),
        }
        let second = reader.read_message().expect("read").expect("message");
        match second {
            Message::Response { id, .. } => assert_eq!(id, 2),
            other => panic!("unexpected message: {other:?}"),
        }
        assert!(reader.read_message().expect("read").is_none());
    }

    #[test]
    fn reads_header_delimited_messages() {
        let body = b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":null}";
        let input = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut bytes = input.into_bytes();
        bytes.extend_from_slice(body);
        let mut reader = MessageReader::new(Cursor::new(bytes), Framing::HeaderDelimited);
        match reader.read_message().expect("read").expect("message") {
            Message::Response { id, .. } => assert_eq!(id, 3),
            other => panic!("unexpected message: {other:?}"),
        }
        assert!(reader.read_message().expect("read").is_none());
    }

    #[test]
    fn recognises_a_call_from_the_plugin() {
        let value = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "UpdateResults",
            "params": ["hello", {"id": 0, "result": []}]
        });
        match Message::from_value(value).expect("message") {
            Message::Call { id, method, params } => {
                assert_eq!(id, Some(9));
                assert_eq!(method, "UpdateResults");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], "hello");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn reads_a_result_from_the_plugin() {
        let value = json!({
            "title": "Chrome",
            "subTitle": "C:\\Program Files\\Chrome",
            "score": 42,
            "icoPath": "Images\\icon.png",
            "titleHighlightData": [0, 3],
            "contextData": { "id": 7 },
            "preview": { "description": "hello", "contentType": "markdown" }
        });
        let result = result_from_json(&value, None);
        assert_eq!(result.title, "Chrome");
        assert_eq!(result.subtitle, "C:\\Program Files\\Chrome");
        assert_eq!(result.score, 42);
        assert_eq!(result.title_highlight, vec![(0, 3)]);
        assert_eq!(result.context_data.as_deref(), Some("{\"id\":7}"));
        let preview = result.preview.expect("preview");
        assert_eq!(preview.description, "hello");
        assert_eq!(preview.content_type, crate::result::PreviewContentType::Markdown);
    }

    #[test]
    fn a_relative_icon_path_is_resolved_against_the_plugin_directory() {
        let value = json!({ "title": "x", "icoPath": "Images\\icon.png" });
        let result = result_from_json(&value, Some(std::path::Path::new(r"C:\plugins\demo")));
        match result.icon {
            ResultIcon::Path(path) => {
                assert!(path.ends_with(r"Images\icon.png"), "{path:?}");
            }
            other => panic!("unexpected icon: {other:?}"),
        }
    }

    #[test]
    fn reads_the_query_envelope() {
        let value = json!({
            "id": 0,
            "result": [{ "title": "one" }, { "title": "two" }],
            "debugMessage": "hi"
        });
        let response = QueryResponse::from_json(&value, None);
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.results[1].title, "two");
        assert_eq!(response.debug_message, "hi");
    }

    #[test]
    fn a_query_is_sent_with_the_shape_the_plugin_expects() {
        let query = Query::plain("hello world");
        let value = query_to_json(&query);
        assert_eq!(value["search"], "hello world");
        assert_eq!(value["trimmedQuery"], "hello world");
        assert_eq!(value["searchTerms"][0], "hello");
        assert_eq!(value["isHomeQuery"], false);
    }
}
