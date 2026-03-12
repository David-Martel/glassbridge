#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::{Manager, State};

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
    process: Option<Child>,
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
    let state = DesktopState::launch().unwrap_or_else(|error| {
        panic!("desktop bootstrap failed: {error}");
    });

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![desktop_bootstrap])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
        ) {
            if let Some(state) = app_handle.try_state::<DesktopState>() {
                state.shutdown();
            }
        }
    });
}

impl DesktopState {
    fn launch() -> Result<Self, String> {
        let repo_root = repo_root()?;
        let backend_entry = repo_root.join("apps").join("server").join("dist").join("index.js");
        if !backend_entry.exists() {
            return Err(format!(
                "Missing backend bundle at {}. Run the desktop launcher after building the server.",
                backend_entry.display()
            ));
        }

        let port = reserve_port()?;
        let api_origin = format!("http://127.0.0.1:{port}");
        let ws_url = format!("ws://127.0.0.1:{port}/ws");
        let data_root = repo_root.join("data");
        fs::create_dir_all(&data_root).map_err(|error| {
            format!(
                "Failed to create desktop data root {}: {error}",
                data_root.display()
            )
        })?;

        let mut command = Command::new(std::env::var("PIXEL_NODE_PATH").unwrap_or_else(|_| "node".into()));
        command
            .current_dir(&repo_root)
            .arg(&backend_entry)
            .env("HOST", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("ANDROID_CONTROL_DATA_ROOT", &data_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command
            .spawn()
            .map_err(|error| format!("Failed to launch backend server: {error}"))?;

        wait_for_health(&api_origin)?;

        Ok(Self {
            runtime: Mutex::new(DesktopRuntime {
                process: Some(child),
                api_origin: api_origin.clone(),
                ws_url,
                launch_target: api_origin,
            }),
        })
    }

    fn shutdown(&self) {
        if let Ok(mut runtime) = self.runtime.lock() {
            if let Some(child) = runtime.process.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            runtime.process = None;
        }
    }
}

fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .canonicalize()
        .map_err(|error| format!("Failed to resolve repository root: {error}"))
}

fn reserve_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("Failed to reserve a localhost port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Failed to read reserved localhost port: {error}"))?
        .port();
    drop(listener);
    Ok(port)
}

fn wait_for_health(api_origin: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("Failed to create backend health client: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);

    while Instant::now() < deadline {
      match client.get(format!("{api_origin}/api/health")).send() {
          Ok(response) if response.status().is_success() => return Ok(()),
          _ => thread::sleep(Duration::from_millis(250)),
      }
    }

    Err(format!(
        "The backend server did not become healthy at {api_origin} within 20 seconds."
    ))
}
