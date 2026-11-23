//! End-to-end test of the JSON-RPC plugin host.
//!
//! A real Python plugin is started as a separate process and driven over the
//! JSON-RPC protocol. The plugin implements the protocol itself, so the test
//! proves that the host side works on its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ideas_launcher::plugins::jsonrpc::{JsonRpcPlugin, NoopHostApi, PluginMetadata};
use launcher_core::plugin::{PluginContext, QueryPlugin};
use launcher_core::query::Query;

/// A real Python interpreter, if this machine has one.
///
/// The Microsoft Store ships a `python.exe` stub that only prints an error,
/// so the candidates are run rather than merely looked for.
fn python() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("Programs").join("Python");
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.join("python.exe").exists())
                .collect();
            versions.sort();
            versions.reverse();
            candidates.extend(versions.into_iter().map(|p| p.join("python.exe")));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| {
            std::process::Command::new(candidate)
                .arg("--version")
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false)
        })
}

/// Set up a plugin directory with the test plugin in it.
fn prepare_plugin() -> Option<PathBuf> {
    let directory = std::env::temp_dir().join("ideas-launcher-jsonrpc-e2e");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create the plugin directory");

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo_plugin.py");
    std::fs::copy(&fixture, directory.join("echo_plugin.py")).expect("copy the fixture");

    std::fs::write(
        directory.join("plugin.json"),
        r#"{
            "ID": "echo",
            "Name": "Echo",
            "Version": "1.0.0",
            "Language": "Python_v2",
            "Description": "a plugin for the end-to-end test",
            "ActionKeywords": ["echo"],
            "ExecuteFileName": "echo_plugin.py"
        }"#,
    )
    .expect("write the metadata");

    Some(directory)
}

#[test]
fn a_python_plugin_is_driven_over_the_protocol() {
    // A panic in the plugin's start-up thread would be invisible otherwise.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("PANIC: {info}");
        previous(info);
    }));

    if python().is_none() {
        eprintln!("no Python on this machine — the end-to-end test is skipped");
        return;
    }

    let Some(directory) = prepare_plugin() else {
        return;
    };

    let metadata = PluginMetadata::read(&directory).expect("the metadata is read");
    assert_eq!(metadata.id, "echo");
    assert_eq!(metadata.language, "Python_v2");

    let plugin = Arc::new(JsonRpcPlugin::new(metadata, Arc::new(NoopHostApi)));
    let context = PluginContext::new("echo", std::env::temp_dir(), std::env::temp_dir());
    plugin.init(&context).expect("the plugin starts");

    // The plugin has to come up before it can answer.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !plugin.is_ready() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        plugin.is_ready(),
        "the plugin should have come up: {:?}",
        plugin.start_error()
    );

    // The query is answered over the wire.
    let results = plugin
        .query(&Query::plain("hello"))
        .expect("the plugin answers");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "echo: hello");
    assert_eq!(results[0].plugin_id, "echo");

    plugin.stop();
    let _ = std::fs::remove_dir_all(&directory);
}
