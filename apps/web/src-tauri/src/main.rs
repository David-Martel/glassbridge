// GlassBridge Desktop — Tauri v2 shell with embedded Rust server
//
// The Axum HTTP/WebSocket server runs in-process, eliminating the Node.js
// child process spawn that was previously required.  The Node.js backend at
// apps/server/ is now legacy and used only for browser-mode development
// (`npm run dev`).
//
// DEPRECATED: The previous architecture spawned `node dist/index.js` as a
// child process and polled /api/health for up to 20 seconds.  That code has
// been removed in favour of the embedded server below.

// Phase 2-5 modules are wired in but not all public APIs are called from
// routes yet — suppress dead_code until every handler is hooked up.
#![allow(dead_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adb;
mod adb_mock;
#[cfg(feature = "native-adb")]
#[allow(dead_code)]
mod adb_native;
mod agent_bus;
mod config;
mod file_transfer;
mod help;
mod logcat;
mod property_cache;
mod registry;
mod server;
mod telemetry;
mod terminal;
mod tooling;
mod wifi_adb;

use serde::Serialize;
use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::State;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DesktopBootstrapPayload {
    api_origin: String,
    ws_url: String,
    launch_target: String,
}

struct DesktopState {
    runtime: Mutex<DesktopRuntime>,
}

struct DesktopRuntime {
    api_origin: String,
    ws_url: String,
    launch_target: String,
}

#[tauri::command]
fn desktop_bootstrap(state: State<'_, DesktopState>) -> Result<DesktopBootstrapPayload, String> {
    let runtime = state
        .runtime
        .lock()
        .map_err(|_| String::from("Desktop state was poisoned"))?;

    Ok(DesktopBootstrapPayload {
        api_origin: runtime.api_origin.clone(),
        ws_url: runtime.ws_url.clone(),
        launch_target: runtime.launch_target.clone(),
    })
}

fn main() {
    // Build a multi-threaded Tokio runtime for the embedded Axum server.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime");

    let _guard = rt.enter();
    let state = launch_embedded_server().unwrap_or_else(|error| {
        panic!("desktop bootstrap failed: {error}");
    });

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![desktop_bootstrap])
        .build(tauri::generate_context!())
        .expect("error while building Tauri application");

    app.run(|_app_handle, _event| {
        // No shutdown handler needed — the Axum server lives in a Tokio task
        // that terminates when the process exits.  No child processes to kill.
    });
}

/// Start the embedded Axum server on a dynamically reserved port.
///
/// Returns a [`DesktopState`] with the resolved URLs.  The server runs in a
/// background Tokio task — there is no Node.js process and no health-check
/// polling.
fn launch_embedded_server() -> Result<DesktopState, String> {
    let repo_root = repo_root()?;
    let cfg = config::Config::from_env(&repo_root);

    // Ensure data directory exists for SQLite registry.
    fs::create_dir_all(&cfg.data_root).map_err(|e| {
        format!(
            "Failed to create data root {}: {e}",
            cfg.data_root.display()
        )
    })?;

    // Reserve a port for the embedded server.
    let port = reserve_port()?;
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let api_origin = format!("http://127.0.0.1:{port}");
    let ws_url = format!("ws://127.0.0.1:{port}/ws");

    // Build the application state (opens SQLite, creates ADB backend).
    let base_state =
        server::AppState::new(cfg.vendor_root.clone(), &cfg.data_root, repo_root.clone())
            .map_err(|e| format!("Failed to initialise app state: {e}"))?;

    // Attempt to connect to the agent-bus (best-effort; None if not running).
    // `launch_embedded_server` is synchronous but runs within `rt.enter()`, so
    // we can drive async code via `block_on` on the current runtime handle.
    let bus = tokio::runtime::Handle::current()
        .block_on(agent_bus::AgentBusClient::try_connect("glassbridge"));
    if let Some(ref b) = bus {
        let api_msg = format!("GlassBridge desktop started — API on {api_origin}");
        tokio::runtime::Handle::current().block_on(async {
            let _ = b
                .set_presence("online", &["adb", "device-monitor", "wifi-adb", "logcat"])
                .await;
            let _ = b
                .post_message("all", "status", &api_msg, &["repo:pixel", "glassbridge"])
                .await;
        });
    }

    let app_state = base_state.with_agent_bus(bus);

    // Spawn the Axum server in a background task.  It runs until the process
    // exits — no graceful shutdown needed because there are no child processes
    // to reap.
    tokio::spawn(async move {
        if let Err(e) = server::serve(app_state, addr).await {
            eprintln!("Embedded server error: {e}");
        }
    });

    Ok(DesktopState {
        runtime: Mutex::new(DesktopRuntime {
            api_origin: api_origin.clone(),
            ws_url,
            launch_target: api_origin,
        }),
    })
}

fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .canonicalize()
        .map_err(|e| format!("Failed to resolve repository root: {e}"))
}

fn reserve_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Failed to reserve a localhost port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read reserved port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}
