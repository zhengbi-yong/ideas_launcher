//! Core domain model for the Rust quick launcher.
//!
//! The shapes here were derived from the reverse engineering of Flow Launcher
//! 2.1.4 (`Flow.Launcher.Plugin.dll` / `Flow.Launcher.Core.dll`) and re-expressed
//! as idiomatic, allocation-conscious Rust. See `analysis/REPORT.md`.

pub mod acronym;
pub mod cancel;
pub mod engine;
pub mod hash;
pub mod history;
pub mod jsonrpc;
pub mod matcher;
pub mod plugin;
pub mod polyphone;
pub mod pinyin;
pub mod profile;
pub mod query;
pub mod regex;
pub mod result;
pub mod search_syntax;
pub mod service;
pub mod settings;
pub mod theme;

pub use engine::{Engine, QueryOutcome};
pub use history::{History, HistoryItem, SelectionMemory};
pub use jsonrpc::{Framing, Message, QueryResponse as JsonRpcQueryResponse};
pub use matcher::{MatchResult, MatchOptions, SearchPrecision, StringMatcher};
pub use plugin::{PluginContext, PluginError, PluginResult, QueryPlugin};
pub use query::{ActionKeywordIndex, Query, QueryBuilder};
pub use result::{
    result_group, PreviewContentType, PreviewData, PreviewVisibility, Result as LauncherResult,
    ResultIcon, ResultOrder,
};
pub use search_syntax::{CompareOp, Node, ParsedSearch, SearchOptions, SearchTarget};
pub use service::{FileHit, Request, Response, Status};
pub use settings::Settings;
pub use theme::{Theme, ThemeKind};
