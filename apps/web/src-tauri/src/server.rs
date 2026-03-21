// DEPRECATED: apps/server/src/index.ts — replaced by this native Rust server
//
// Embedded Axum HTTP + WebSocket server for GlassBridge.
//
// This module owns the full API surface previously served by the Node.js /
// Fastify backend.  It is started as a background Tokio task from `main.rs`
// and exposes the same route structure so the React frontend requires zero
// changes.
//
// ## Dependency on sibling modules
//
// This module imports concrete types from `crate::adb`, `crate::registry`, and
// `crate::tooling`:
//
// - `crate::adb`:      `AdbProcessBackend`, `DeviceRecord`, `CommandResult`,
//                      `ScrcpyPreset`, `DeviceOps`, `AdbError`
// - `crate::registry`: `Registry`, `RegistryRecord`, `RegistrySummary`,
//                      `RegistryMapEntry`, `ApproveOptions`, `RegistryError`
// - `crate::tooling`:  `get_tool_status`, `ToolStatus`, `ToolInfo`,
//                      `ToolError`
//
// Note: `registry::DeviceRecord` and `adb::DeviceRecord` are structurally
// identical but are different types.  The `adb_to_registry_device` helper
// converts between them so callers do not need to know this detail.
//
// ## Architecture pattern (M-SERVICES-CLONE)
//
// `AppState` wraps all shared resources in `Arc` so cloning the state — as
// Axum does for every handler dispatch — is a reference-count bump rather
// than a deep copy.  The `Registry` is guarded by a `tokio::sync::Mutex`
// because `rusqlite::Connection` is `!Send`.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        Path as AxumPath, State, WebSocketUpgrade,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};

use crate::{
    adb::{AdbError, CommandResult, DeviceOps, DeviceRecord, ScrcpyPreset},
    agent_bus::AgentBusClient,
    file_transfer::{FileTransfer, TransferError},
    help::help_topics,
    logcat::{LogcatFilter, LogcatStream},
    property_cache::{CacheError, PropertyCache},
    registry::{ApproveOptions, Registry, RegistryError, RegistryMapEntry, RegistrySummary},
    telemetry::{TelemetryCollector, TimedOperation},
    tooling::{self, ToolError, ToolInfo, ToolStatus},
    wifi_adb::{WifiAdb, WifiAdbError},
};

// Suppress unused-import warnings for items only used under feature gates.
#[allow(unused_imports)]
use crate::adb::create_adb_backend;

// ---------------------------------------------------------------------------
// Type conversion — adb::DeviceRecord → registry::DeviceRecord
// ---------------------------------------------------------------------------

/// Convert an `adb::DeviceRecord` into the structurally-identical
/// `registry::DeviceRecord`.  Both types have the same public fields but live
/// in different modules; this prevents coupling `registry` on `adb`.
fn adb_to_registry_device(d: &DeviceRecord) -> crate::registry::DeviceRecord {
    crate::registry::DeviceRecord {
        serial: d.serial.clone(),
        state: d.state.clone(),
        model: d.model.clone(),
        product: d.product.clone(),
        device: d.device.clone(),
        transport_id: d.transport_id.clone(),
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// All errors that the HTTP handlers can surface to the client.
///
/// Every variant serialises to `{ "error": "<message>" }` through the shared
/// `IntoResponse` implementation, preserving the existing Node.js error shape.
#[derive(Debug, Error)]
pub enum ServerError {
    /// A device serial supplied by the caller was not found in the ADB list.
    #[error("Connected device {serial} was not found")]
    DeviceNotFound { serial: String },

    /// A request body failed validation (e.g. empty `packageName`).
    #[error("Invalid request body: {0}")]
    InvalidBody(String),

    /// An underlying ADB operation failed.
    #[error("ADB error: {0}")]
    Adb(#[from] AdbError),

    /// An underlying registry operation failed.
    #[error("Registry error: {0}")]
    Registry(#[from] RegistryError),

    /// A tooling operation failed.
    #[error("Tooling error: {0}")]
    Tooling(#[from] ToolError),

    /// A file transfer operation (push/pull) failed.
    #[error("File transfer error: {0}")]
    Transfer(#[from] TransferError),

    /// A device property cache fetch failed.
    #[error("Property cache error: {0}")]
    Cache(#[from] CacheError),

    /// A WiFi ADB operation failed.
    #[error("WiFi ADB error: {0}")]
    WifiAdb(#[from] WifiAdbError),

    /// A logcat stream operation failed.
    #[error("Logcat error: {0}")]
    Logcat(String),
}

#[allow(dead_code)]
impl ServerError {
    /// Returns `true` when the error originates from a device not being found.
    #[inline]
    #[must_use]
    pub fn is_device_not_found(&self) -> bool {
        matches!(self, Self::DeviceNotFound { .. })
    }

    /// Returns `true` when the error wraps an `AdbError::UnknownAction`.
    #[inline]
    #[must_use]
    pub fn is_unsupported_action(&self) -> bool {
        matches!(self, Self::Adb(AdbError::UnknownAction { .. }))
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = match &self {
            ServerError::DeviceNotFound { .. } => StatusCode::BAD_REQUEST,
            ServerError::Adb(AdbError::UnknownAction { .. }) => StatusCode::BAD_REQUEST,
            ServerError::InvalidBody(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ServerError::WifiAdb(WifiAdbError::InvalidAddress { .. })
            | ServerError::WifiAdb(WifiAdbError::InvalidPairingCode { .. }) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ServerError::Transfer(TransferError::LocalPath(_)) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        #[derive(Serialize)]
        struct ErrorBody {
            error: String,
        }

        (
            status,
            Json(ErrorBody {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Wire types — JSON shapes the frontend reads
// ---------------------------------------------------------------------------

/// Registry-enriched view of a device, matching `enrichDevice()` in `index.ts`.
///
/// The `device` fields are inlined via `#[serde(flatten)]` so the JSON object
/// has the flat shape the frontend expects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedDevice {
    #[serde(flatten)]
    pub device: DeviceRecord,
    pub alias: Option<String>,
    pub approval_status: String,
    pub approved: bool,
    pub notes: Option<String>,
    pub first_seen_at: Option<String>,
    pub last_seen_at: Option<String>,
}

/// Per-tool wire shape for `GET /api/tools/status`, matching `getToolStatus()`
/// from `tooling.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireToolEntry {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

impl From<ToolInfo> for WireToolEntry {
    fn from(info: ToolInfo) -> Self {
        Self {
            installed: info.installed,
            path: info.path.map(|p| p.to_string_lossy().into_owned()),
            version: info.version,
        }
    }
}

/// Combined tool-status wire shape, matching `getToolStatus()` JSON output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireToolStatus {
    pub vendor_root: String,
    pub adb: WireToolEntry,
    pub scrcpy: WireToolEntry,
}

impl From<ToolStatus> for WireToolStatus {
    fn from(ts: ToolStatus) -> Self {
        Self {
            vendor_root: ts.vendor_root.to_string_lossy().into_owned(),
            adb: WireToolEntry::from(ts.adb),
            scrcpy: WireToolEntry::from(ts.scrcpy),
        }
    }
}

// ---------------------------------------------------------------------------
// ScrcpyPreset Serialize
//
// `adb.rs` only derives `Deserialize` on `ScrcpyPreset`; we add `Serialize`
// here so the `POST /api/devices/{serial}/scrcpy` response can echo back the
// preset value.  We cannot modify `adb.rs` from here, so we implement the
// trait manually.
// ---------------------------------------------------------------------------

impl Serialize for ScrcpyPreset {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(match self {
            ScrcpyPreset::Latency => "latency",
            ScrcpyPreset::Balanced => "balanced",
            ScrcpyPreset::Quality => "quality",
        })
    }
}

// ---------------------------------------------------------------------------
// Shared application state (M-SERVICES-CLONE)
// ---------------------------------------------------------------------------

/// Application state shared across all Axum handlers.
///
/// Cloning is cheap: all expensive resources live behind `Arc`.  The
/// `tokio::sync::Mutex` guard around `Registry` serialises SQLite writes
/// because `rusqlite::Connection` is `!Send`.
#[derive(Clone)]
pub struct AppState {
    /// ADB operations backend.  Production: uses `create_adb_backend` factory.
    pub adb: Arc<dyn DeviceOps + Send + Sync>,
    /// Persistent SQLite-backed device registry.
    pub registry: Arc<Mutex<Registry>>,
    /// Vendored binary root for adb / scrcpy tool resolution.
    pub vendor_root: PathBuf,
    /// Repository root used to locate the bootstrap script.
    pub repo_root: PathBuf,
    /// Per-device cached properties (model, screen size, battery, etc.).
    pub property_cache: PropertyCache,
    /// Performance telemetry ring-buffer for ADB operations.
    pub telemetry: Arc<TelemetryCollector>,
    /// Map of device serial → active logcat stream.
    pub logcat_streams: Arc<Mutex<HashMap<String, LogcatStream>>>,
    /// WiFi ADB helper (connect / disconnect / pair / tcpip).
    pub wifi_adb: WifiAdb,
    /// Optional agent-bus HTTP client — `None` when the bus is not running.
    pub agent_bus: Option<AgentBusClient>,
}

impl AppState {
    /// Construct an [`AppState`] from concrete paths.
    ///
    /// Opens the SQLite registry at `data_root/device-registry.db`.
    /// Uses [`create_adb_backend`] so the native TCP backend is preferred when
    /// the `native-adb` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] when the SQLite database cannot be opened or
    /// the schema cannot be applied.
    pub fn new(
        vendor_root: PathBuf,
        data_root: &Path,
        repo_root: PathBuf,
    ) -> Result<Self, RegistryError> {
        let registry = Registry::open(data_root)?;
        Ok(Self {
            adb: crate::adb::create_adb_backend(&vendor_root),
            registry: Arc::new(Mutex::new(registry)),
            wifi_adb: WifiAdb::new(&vendor_root),
            vendor_root,
            repo_root,
            property_cache: PropertyCache::new(),
            telemetry: Arc::new(TelemetryCollector::new()),
            logcat_streams: Arc::new(Mutex::new(HashMap::new())),
            agent_bus: None,
        })
    }

    /// Return a new [`AppState`] with the given `agent_bus` client wired in.
    ///
    /// Designed to be called from `launch_embedded_server` after an async
    /// `AgentBusClient::try_connect` — avoids making `new` itself async.
    #[must_use]
    pub fn with_agent_bus(mut self, bus: Option<AgentBusClient>) -> Self {
        self.agent_bus = bus;
        self
    }
}

// Convenience: allow tests to construct AppState directly with a stub ADB.
impl AppState {
    #[cfg(test)]
    pub(crate) fn new_with_adb(
        adb: Arc<dyn DeviceOps + Send + Sync>,
        registry: Registry,
        vendor_root: PathBuf,
        repo_root: PathBuf,
    ) -> Self {
        Self {
            wifi_adb: WifiAdb::new(&vendor_root),
            adb,
            registry: Arc::new(Mutex::new(registry)),
            vendor_root,
            repo_root,
            property_cache: PropertyCache::new(),
            telemetry: Arc::new(TelemetryCollector::new()),
            logcat_streams: Arc::new(Mutex::new(HashMap::new())),
            agent_bus: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Router construction
// ---------------------------------------------------------------------------

/// Build the Axum [`Router`] with all GlassBridge API routes.
///
/// The CORS layer permits all origins, matching the Node.js `@fastify/cors
/// { origin: true }` option.  Static serving of the React dist is intentionally
/// omitted — the caller layers `tower_http::services::ServeDir` as a fallback
/// below this router.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::{Path, PathBuf};
/// use glassbridge_desktop::server::{build_router, AppState};
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let state = AppState::new(
///     PathBuf::from("/repo/vendor"),
///     Path::new("/data"),
///     PathBuf::from("/repo"),
/// )?;
/// let router = build_router(state);
/// # Ok(())
/// # }
/// ```
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health
        .route("/api/health", get(health_handler))
        // Devices
        .route("/api/devices", get(list_devices_handler))
        .route("/api/devices/{serial}/apps", get(list_apps_handler))
        .route("/api/devices/{serial}/screenshot", get(screenshot_handler))
        .route("/api/devices/{serial}/control", post(control_handler))
        .route("/api/devices/{serial}/tap", post(tap_handler))
        .route("/api/devices/{serial}/text", post(text_handler))
        .route("/api/devices/{serial}/apps/start", post(app_start_handler))
        .route("/api/devices/{serial}/apps/stop", post(app_stop_handler))
        .route("/api/devices/{serial}/shell", post(shell_handler))
        .route("/api/devices/{serial}/scrcpy", post(scrcpy_handler))
        // Approved devices
        .route("/api/approved-devices", get(list_approved_devices_handler))
        .route(
            "/api/approved-devices/{serial}/approve",
            post(approve_device_handler),
        )
        // Tools
        .route("/api/tools/status", get(tools_status_handler))
        .route("/api/tools/bootstrap", post(tools_bootstrap_handler))
        // Help
        .route("/api/help/topics", get(help_topics_handler))
        // Logcat (start/stop + poll recent entries)
        .route(
            "/api/devices/{serial}/logcat/start",
            post(logcat_start_handler),
        )
        .route(
            "/api/devices/{serial}/logcat/stop",
            post(logcat_stop_handler),
        )
        .route(
            "/api/devices/{serial}/logcat/entries",
            get(logcat_entries_handler),
        )
        // File transfer
        .route("/api/devices/{serial}/push", post(push_handler))
        .route("/api/devices/{serial}/pull", post(pull_handler))
        // Device properties (cached)
        .route("/api/devices/{serial}/properties", get(properties_handler))
        // Telemetry
        .route("/api/telemetry", get(telemetry_handler))
        // WiFi ADB
        .route("/api/wifi/connect", post(wifi_connect_handler))
        .route("/api/wifi/disconnect", post(wifi_disconnect_handler))
        .route("/api/wifi/pair", post(wifi_pair_handler))
        .route("/api/wifi/tcpip", post(wifi_tcpip_handler))
        .route("/api/wifi/devices", get(wifi_devices_handler))
        // WebSocket snapshot feed
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Bind `addr`, start the Axum server, and block until it shuts down.
///
/// Intended to be called inside `tokio::spawn` from `main.rs`.
///
/// # Errors
///
/// Returns `Err` if the TCP listener cannot bind to `addr` or Axum reports a
/// fatal serve error.
pub async fn serve(state: AppState, addr: SocketAddr) -> Result<(), std::io::Error> {
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("GlassBridge API server listening on http://{addr}");
    axum::serve(listener, router).await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Enrich a live `adb::DeviceRecord` with its `RegistryMapEntry` overlay.
///
/// Mirrors `enrichDevice()` from `index.ts`: absent overlay → `"candidate"`.
fn enrich(device: DeviceRecord, overlay: Option<&RegistryMapEntry>) -> EnrichedDevice {
    let approval_status = overlay
        .map(|o| o.approval_status.clone())
        .unwrap_or_else(|| "candidate".to_owned());
    let approved = approval_status == "approved";
    EnrichedDevice {
        alias: overlay.and_then(|o| o.alias.clone()),
        approval_status,
        approved,
        notes: overlay.and_then(|o| o.notes.clone()),
        first_seen_at: overlay.map(|o| o.first_seen_at.clone()),
        last_seen_at: overlay.map(|o| o.last_seen_at.clone()),
        device,
    }
}

/// List all connected devices, sync them into the registry, and return the
/// enriched view.
///
/// Mirrors `snapshotDevices()` from `index.ts`.
async fn snapshot_devices(state: &AppState) -> Result<Vec<EnrichedDevice>, ServerError> {
    let adb_devices: Vec<DeviceRecord> = state.adb.list_devices().await?;

    let reg = state.registry.lock().await;
    let reg_devices: Vec<crate::registry::DeviceRecord> =
        adb_devices.iter().map(adb_to_registry_device).collect();
    reg.sync_seen_devices(&reg_devices)?;
    let overlay_map = reg.get_registry_map()?;

    Ok(adb_devices
        .into_iter()
        .map(|d| {
            let overlay = overlay_map.get(&d.serial);
            enrich(d, overlay)
        })
        .collect())
}

/// Build the full WebSocket snapshot payload, tolerating partial failures.
///
/// Matches the `setInterval` payload structure from `index.ts`:
/// `{ type, tools, devices, approvedDevices, registry }`.
async fn build_snapshot(state: &AppState) -> serde_json::Value {
    // Tool status never fails (returns defaults for missing tools).
    let tool_status = tooling::get_tool_status(&state.vendor_root).await;

    let (devices_json, approved_json, registry_json) = {
        let reg = state.registry.lock().await;

        match state.adb.list_devices().await {
            Ok(adb_devices) => {
                let reg_devices: Vec<crate::registry::DeviceRecord> =
                    adb_devices.iter().map(adb_to_registry_device).collect();
                let _ = reg.sync_seen_devices(&reg_devices); // best-effort

                let overlay_map = reg.get_registry_map().unwrap_or_default();
                let enriched: Vec<EnrichedDevice> = adb_devices
                    .into_iter()
                    .map(|d| {
                        let overlay = overlay_map.get(&d.serial);
                        enrich(d, overlay)
                    })
                    .collect();

                let approved = reg.list_approved_devices().unwrap_or_default();
                let summary = reg.get_registry_summary().unwrap_or(RegistrySummary {
                    approved_count: 0,
                    known_count: 0,
                });

                (
                    serde_json::to_value(&enriched).unwrap_or(serde_json::Value::Array(vec![])),
                    serde_json::to_value(&approved).unwrap_or(serde_json::Value::Array(vec![])),
                    serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null),
                )
            }
            Err(err) => {
                warn!("ws snapshot: list_devices failed: {err}");
                (
                    serde_json::Value::Array(vec![]),
                    serde_json::Value::Array(vec![]),
                    serde_json::Value::Null,
                )
            }
        }
    };

    serde_json::json!({
        "type": "snapshot",
        "tools": WireToolStatus::from(tool_status),
        "devices": devices_json,
        "approvedDevices": approved_json,
        "registry": registry_json,
    })
}

/// Read `vendor_root/tool-manifest.json` as a raw JSON value.
///
/// Returns `null` when the file is absent or cannot be parsed, matching
/// `readToolManifest()` from `tooling.ts`.
async fn read_tool_manifest_json(vendor_root: &std::path::Path) -> serde_json::Value {
    let path = vendor_root.join("tool-manifest.json");
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

// GET /api/health
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

// GET /api/devices
async fn list_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let mut timer = TimedOperation::start(Arc::clone(&state.telemetry), "list_devices", "process");
    let result = snapshot_devices(&state).await;
    if result.is_err() {
        timer.fail();
    }
    let devices = result?;
    Ok(Json(serde_json::json!({ "devices": devices })))
}

// GET /api/devices/{serial}/apps
async fn list_apps_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let packages = state.adb.list_packages(&serial).await?;
    Ok(Json(serde_json::json!({ "packages": packages })))
}

// GET /api/devices/{serial}/screenshot
async fn screenshot_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
) -> Result<Response, ServerError> {
    let mut timer = TimedOperation::start(Arc::clone(&state.telemetry), "screenshot", "process");
    let result = state.adb.capture_screenshot(&serial).await;
    if result.is_err() {
        timer.fail();
    }
    let png: Vec<u8> = result?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Bytes::from(png),
    )
        .into_response())
}

// POST /api/devices/{serial}/control  — body: { action: string }
#[derive(Deserialize)]
struct ControlBody {
    action: String,
}

async fn control_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<ControlBody>,
) -> Result<Json<CommandResult>, ServerError> {
    Ok(Json(state.adb.send_key(&serial, &body.action).await?))
}

// POST /api/devices/{serial}/tap  — body: { x: f64, y: f64 }
#[derive(Deserialize)]
struct TapBody {
    x: f64,
    y: f64,
}

async fn tap_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<TapBody>,
) -> Result<Json<CommandResult>, ServerError> {
    Ok(Json(state.adb.tap(&serial, body.x, body.y).await?))
}

// POST /api/devices/{serial}/text  — body: { text: string }
#[derive(Deserialize)]
struct TextBody {
    text: String,
}

async fn text_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<TextBody>,
) -> Result<Json<CommandResult>, ServerError> {
    Ok(Json(state.adb.input_text(&serial, &body.text).await?))
}

// POST /api/devices/{serial}/apps/start  — body: { packageName: string }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageBody {
    package_name: String,
}

async fn app_start_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<PackageBody>,
) -> Result<Json<CommandResult>, ServerError> {
    if body.package_name.is_empty() {
        return Err(ServerError::InvalidBody(
            "packageName must not be empty".to_owned(),
        ));
    }
    Ok(Json(
        state
            .adb
            .launch_package(&serial, &body.package_name)
            .await?,
    ))
}

// POST /api/devices/{serial}/apps/stop  — body: { packageName: string }
async fn app_stop_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<PackageBody>,
) -> Result<Json<CommandResult>, ServerError> {
    if body.package_name.is_empty() {
        return Err(ServerError::InvalidBody(
            "packageName must not be empty".to_owned(),
        ));
    }
    Ok(Json(
        state.adb.stop_package(&serial, &body.package_name).await?,
    ))
}

// POST /api/devices/{serial}/shell  — body: { command: string }
#[derive(Deserialize)]
struct ShellBody {
    command: String,
}

async fn shell_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<ShellBody>,
) -> Result<Json<CommandResult>, ServerError> {
    if body.command.is_empty() {
        return Err(ServerError::InvalidBody(
            "command must not be empty".to_owned(),
        ));
    }
    let mut timer = TimedOperation::start(Arc::clone(&state.telemetry), "shell", "process");
    let result = state.adb.shell_command(&serial, &body.command).await;
    if result.is_err() {
        timer.fail();
    }
    Ok(Json(result?))
}

// POST /api/devices/{serial}/scrcpy  — body: { preset: "latency"|"balanced"|"quality" }
#[derive(Deserialize)]
struct ScrcpyBody {
    preset: ScrcpyPreset,
}

/// Response body for `POST /api/devices/{serial}/scrcpy`, matching `{ ok, preset }`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScrcpyResponse {
    ok: bool,
    preset: ScrcpyPreset,
}

async fn scrcpy_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<ScrcpyBody>,
) -> Result<Json<ScrcpyResponse>, ServerError> {
    let preset = body.preset;
    state.adb.start_scrcpy(&serial, preset.clone()).await?;
    Ok(Json(ScrcpyResponse { ok: true, preset }))
}

// GET /api/approved-devices
async fn list_approved_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let devices = state.registry.lock().await.list_approved_devices()?;
    Ok(Json(serde_json::json!({ "devices": devices })))
}

// POST /api/approved-devices/{serial}/approve  — body: { alias?: string, notes?: string }
#[derive(Deserialize, Default)]
struct ApproveBody {
    alias: Option<String>,
    notes: Option<String>,
}

async fn approve_device_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    // Body is optional — `{}` (or no body) is valid.
    body: Option<Json<ApproveBody>>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let (alias, notes) = body
        .map(|Json(b)| {
            let alias = b
                .alias
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            let notes = b
                .notes
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            (alias, notes)
        })
        .unwrap_or((None, None));

    // Verify the device is currently connected, mirroring the Node.js guard.
    let adb_devices = state.adb.list_devices().await?;
    let target_adb = adb_devices
        .iter()
        .find(|d| d.serial == serial)
        .ok_or_else(|| ServerError::DeviceNotFound {
            serial: serial.clone(),
        })?;
    let target_reg = adb_to_registry_device(target_adb);

    let reg = state.registry.lock().await;
    let reg_devices: Vec<crate::registry::DeviceRecord> =
        adb_devices.iter().map(adb_to_registry_device).collect();
    reg.sync_seen_devices(&reg_devices)?;
    reg.approve_device(
        &target_reg,
        ApproveOptions {
            alias,
            notes,
            approval_source: Some("local-ui".to_owned()),
        },
    )?;

    let approved_device = reg
        .list_approved_devices()?
        .into_iter()
        .find(|d| d.serial == serial);

    Ok(Json(serde_json::json!({
        "ok": true,
        "approvedDevice": approved_device,
    })))
}

// GET /api/tools/status
async fn tools_status_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let raw_status = tooling::get_tool_status(&state.vendor_root).await;
    let wire_status = WireToolStatus::from(raw_status);
    let manifest = read_tool_manifest_json(&state.vendor_root).await;
    let registry_summary = state.registry.lock().await.get_registry_summary()?;

    Ok(Json(serde_json::json!({
        "status": wire_status,
        "manifest": manifest,
        "registry": registry_summary,
    })))
}

// POST /api/tools/bootstrap
async fn tools_bootstrap_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    use tokio::process::Command;

    // Invoke the Node.js bootstrap script exactly as the old Fastify server
    // did: `node tools/bootstrap-tools.mjs`.
    let bootstrap_script = state.repo_root.join("tools").join("bootstrap-tools.mjs");
    let node = std::env::var("PIXEL_NODE_PATH").unwrap_or_else(|_| "node".into());

    let output = Command::new(&node)
        .arg(&bootstrap_script)
        .current_dir(&state.repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(ToolError::ManifestIo)?;

    let raw_status = tooling::get_tool_status(&state.vendor_root).await;

    Ok(Json(serde_json::json!({
        "output": {
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "exitCode": output.status.code().unwrap_or(-1),
        },
        "status": WireToolStatus::from(raw_status),
    })))
}

// GET /api/help/topics
async fn help_topics_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "topics": help_topics() }))
}

// ---------------------------------------------------------------------------
// Logcat handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /api/devices/{serial}/logcat/start`.
///
/// All filter fields are optional; omitting them starts an unfiltered stream.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LogcatStartBody {
    /// Only emit entries from these tags (empty = no tag filter).
    #[serde(default)]
    tags: Vec<String>,
    /// Minimum priority: `"verbose"`, `"debug"`, `"info"`, `"warn"`,
    /// `"error"`, `"fatal"`, or `"silent"`.
    min_priority: Option<String>,
    /// Only emit entries from this PID.
    pid: Option<u32>,
}

// POST /api/devices/{serial}/logcat/start
async fn logcat_start_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    body: Option<Json<LogcatStartBody>>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();

    let min_priority = body.min_priority.as_deref().and_then(|s| {
        use crate::logcat::LogPriority;
        match s {
            "verbose" => Some(LogPriority::Verbose),
            "debug" => Some(LogPriority::Debug),
            "info" => Some(LogPriority::Info),
            "warn" => Some(LogPriority::Warn),
            "error" => Some(LogPriority::Error),
            "fatal" => Some(LogPriority::Fatal),
            "silent" => Some(LogPriority::Silent),
            _ => None,
        }
    });

    let filter = LogcatFilter {
        tags: body.tags,
        min_priority,
        pid: body.pid,
    };

    // Check if a stream is already running for this serial.
    {
        let guard = state.logcat_streams.lock().await;
        if let Some(stream) = guard.get(&serial) {
            if stream.is_running() {
                return Err(ServerError::Logcat(format!(
                    "logcat stream already running for {serial}"
                )));
            }
        }
    }

    let stream = LogcatStream::start(serial.clone(), &state.vendor_root, filter)
        .await
        .map_err(|e| ServerError::Logcat(e.to_string()))?;

    state
        .logcat_streams
        .lock()
        .await
        .insert(serial.clone(), stream);

    info!(serial = %serial, "logcat stream started");
    Ok(Json(serde_json::json!({ "ok": true, "serial": serial })))
}

// POST /api/devices/{serial}/logcat/stop
async fn logcat_stop_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let mut guard = state.logcat_streams.lock().await;
    if let Some(mut stream) = guard.remove(&serial) {
        stream.stop().await;
        info!(serial = %serial, "logcat stream stopped");
        Ok(Json(serde_json::json!({ "ok": true, "serial": serial })))
    } else {
        Ok(Json(
            serde_json::json!({ "ok": false, "serial": serial, "message": "no stream running" }),
        ))
    }
}

// GET /api/devices/{serial}/logcat/entries  — fetch buffered entries via receiver drain
async fn logcat_entries_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let guard = state.logcat_streams.lock().await;
    let Some(stream) = guard.get(&serial) else {
        return Ok(Json(serde_json::json!({
            "serial": serial,
            "running": false,
            "entries": [],
        })));
    };

    // Drain all currently buffered entries from a fresh receiver.
    let mut rx = stream.subscribe();
    let mut entries = Vec::new();

    // `try_recv` returns all already-buffered messages without blocking.
    loop {
        match rx.try_recv() {
            Ok(entry) => entries.push(entry),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
        }
    }

    let running = stream.is_running();
    Ok(Json(serde_json::json!({
        "serial": serial,
        "running": running,
        "entries": entries,
    })))
}

// ---------------------------------------------------------------------------
// File transfer handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /api/devices/{serial}/push`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushBody {
    /// Absolute local filesystem path of the file to push.
    local_path: String,
    /// Absolute remote device path destination.
    remote_path: String,
}

// POST /api/devices/{serial}/push
async fn push_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<PushBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    if body.local_path.is_empty() {
        return Err(ServerError::InvalidBody(
            "localPath must not be empty".into(),
        ));
    }
    if body.remote_path.is_empty() {
        return Err(ServerError::InvalidBody(
            "remotePath must not be empty".into(),
        ));
    }

    FileTransfer::push(
        &serial,
        Path::new(&body.local_path),
        &body.remote_path,
        &state.vendor_root,
        None,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "serial": serial,
        "localPath": body.local_path,
        "remotePath": body.remote_path,
    })))
}

/// Request body for `POST /api/devices/{serial}/pull`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullBody {
    /// Absolute remote device path of the file to pull.
    remote_path: String,
    /// Absolute local filesystem destination path.
    local_path: String,
}

// POST /api/devices/{serial}/pull
async fn pull_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
    Json(body): Json<PullBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    if body.remote_path.is_empty() {
        return Err(ServerError::InvalidBody(
            "remotePath must not be empty".into(),
        ));
    }
    if body.local_path.is_empty() {
        return Err(ServerError::InvalidBody(
            "localPath must not be empty".into(),
        ));
    }

    FileTransfer::pull(
        &serial,
        &body.remote_path,
        Path::new(&body.local_path),
        &state.vendor_root,
        None,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "serial": serial,
        "remotePath": body.remote_path,
        "localPath": body.local_path,
    })))
}

// ---------------------------------------------------------------------------
// Device properties handler
// ---------------------------------------------------------------------------

// GET /api/devices/{serial}/properties
async fn properties_handler(
    State(state): State<AppState>,
    AxumPath(serial): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let props = state
        .property_cache
        .get_or_fetch(&serial, state.adb.as_ref())
        .await?;

    Ok(Json(
        serde_json::to_value(&props).unwrap_or(serde_json::Value::Null),
    ))
}

// ---------------------------------------------------------------------------
// Telemetry handler
// ---------------------------------------------------------------------------

// GET /api/telemetry
async fn telemetry_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let summary = state.telemetry.summary();
    let recent = state.telemetry.recent(50);
    Json(serde_json::json!({
        "summary": summary,
        "recent": recent,
    }))
}

// ---------------------------------------------------------------------------
// WiFi ADB handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /api/wifi/connect`.
#[derive(Deserialize)]
struct WifiAddressBody {
    address: String,
}

// POST /api/wifi/connect
async fn wifi_connect_handler(
    State(state): State<AppState>,
    Json(body): Json<WifiAddressBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let message = state.wifi_adb.connect(&body.address).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "address": body.address,
        "message": message,
    })))
}

// POST /api/wifi/disconnect
async fn wifi_disconnect_handler(
    State(state): State<AppState>,
    Json(body): Json<WifiAddressBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let message = state.wifi_adb.disconnect(&body.address).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "address": body.address,
        "message": message,
    })))
}

/// Request body for `POST /api/wifi/pair`.
#[derive(Deserialize)]
struct WifiPairBody {
    address: String,
    code: String,
}

// POST /api/wifi/pair
async fn wifi_pair_handler(
    State(state): State<AppState>,
    Json(body): Json<WifiPairBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let message = state.wifi_adb.pair(&body.address, &body.code).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "address": body.address,
        "message": message,
    })))
}

/// Request body for `POST /api/wifi/tcpip`.
#[derive(Deserialize)]
struct WifiTcpipBody {
    serial: String,
    #[serde(default = "default_adb_port")]
    port: u16,
}

fn default_adb_port() -> u16 {
    5555
}

// POST /api/wifi/tcpip
async fn wifi_tcpip_handler(
    State(state): State<AppState>,
    Json(body): Json<WifiTcpipBody>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let message = state.wifi_adb.enable_tcpip(&body.serial, body.port).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "serial": body.serial,
        "port": body.port,
        "message": message,
    })))
}

// GET /api/wifi/devices
async fn wifi_devices_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    let devices = state.wifi_adb.list_wifi_devices().await?;
    Ok(Json(serde_json::json!({ "devices": devices })))
}

// ---------------------------------------------------------------------------
// WebSocket handler — broadcasts snapshots every 2 seconds
// ---------------------------------------------------------------------------

// GET /ws  (WebSocket upgrade)
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

/// Drive a single WebSocket connection, pushing snapshots every 2 seconds.
///
/// The loop exits cleanly when the client closes the connection or a send
/// error is detected.  Matches the `setInterval` + socket-set pattern from
/// `index.ts`, adapted to a per-connection task model.
async fn handle_ws_connection(mut socket: WebSocket, state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let payload = build_snapshot(&state).await;
                let text = match serde_json::to_string(&payload) {
                    Ok(t) => t,
                    Err(err) => {
                        error!("ws: failed to serialise snapshot: {err}");
                        serde_json::json!({
                            "type": "error",
                            "message": err.to_string(),
                        })
                        .to_string()
                    }
                };

                if socket.send(Message::Text(text.into())).await.is_err() {
                    break; // client disconnected
                }
            }

            // Detect close frames promptly rather than waiting for the tick.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore pings / data frames from client
                    Some(Err(err)) => {
                        warn!("ws: receive error: {err}");
                        break;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{body::Body, http::Request};
    use tower::util::ServiceExt; // for `oneshot`

    // -----------------------------------------------------------------------
    // In-process stub ADB backend
    // -----------------------------------------------------------------------

    struct StubAdb;

    #[async_trait]
    impl DeviceOps for StubAdb {
        async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AdbError> {
            Ok(vec![DeviceRecord {
                serial: "TEST001".to_owned(),
                state: "device".to_owned(),
                model: Some("Pixel 10 XL".to_owned()),
                product: Some("mustang".to_owned()),
                device: Some("mustang".to_owned()),
                transport_id: Some("1".to_owned()),
            }])
        }

        async fn list_packages(&self, _: &str) -> Result<Vec<String>, AdbError> {
            Ok(vec!["com.example.app".to_owned()])
        }

        async fn capture_screenshot(&self, _: &str) -> Result<Vec<u8>, AdbError> {
            Ok(crate::adb_mock::SCREENSHOT_PNG_BYTES.to_vec())
        }

        async fn send_key(&self, _: &str, action: &str) -> Result<CommandResult, AdbError> {
            if action == "unknown_key" {
                return Err(AdbError::UnknownAction {
                    action: action.to_owned(),
                });
            }
            Ok(stub_result())
        }

        async fn tap(&self, _: &str, _x: f64, _y: f64) -> Result<CommandResult, AdbError> {
            Ok(stub_result())
        }

        async fn input_text(&self, _: &str, _: &str) -> Result<CommandResult, AdbError> {
            Ok(stub_result())
        }

        async fn launch_package(&self, _: &str, _: &str) -> Result<CommandResult, AdbError> {
            Ok(stub_result())
        }

        async fn stop_package(&self, _: &str, _: &str) -> Result<CommandResult, AdbError> {
            Ok(stub_result())
        }

        async fn shell_command(&self, _: &str, _: &str) -> Result<CommandResult, AdbError> {
            Ok(stub_result())
        }

        async fn start_scrcpy(&self, _: &str, _: ScrcpyPreset) -> Result<(), AdbError> {
            Ok(())
        }
    }

    fn stub_result() -> CommandResult {
        CommandResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    fn test_state() -> AppState {
        AppState::new_with_adb(
            Arc::new(StubAdb),
            Registry::open_in_memory().expect("in-memory registry must open"),
            PathBuf::from("/nonexistent/vendor"),
            PathBuf::from("/nonexistent/repo"),
        )
    }

    fn test_router() -> Router {
        build_router(test_state())
    }

    // -----------------------------------------------------------------------
    // HTTP helpers
    // -----------------------------------------------------------------------

    async fn get_json(router: &Router, path: &str) -> (StatusCode, serde_json::Value) {
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let resp: axum::response::Response = router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    async fn post_json(
        router: &Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .uri(path)
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp: axum::response::Response = router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn health_returns_ok_true() {
        let (status, body) = get_json(&test_router(), "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn devices_returns_enriched_array() {
        let (status, body) = get_json(&test_router(), "/api/devices").await;
        assert_eq!(status, StatusCode::OK);
        let device = &body["devices"][0];
        assert_eq!(device["serial"], "TEST001");
        assert_eq!(device["approvalStatus"], "candidate");
        assert_eq!(device["approved"], false);
    }

    #[tokio::test]
    async fn apps_returns_package_list() {
        let (status, body) = get_json(&test_router(), "/api/devices/TEST001/apps").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["packages"][0], "com.example.app");
    }

    #[tokio::test]
    async fn screenshot_returns_png_content_type() {
        let router = test_router();
        let req = Request::builder()
            .uri("/api/devices/TEST001/screenshot")
            .body(Body::empty())
            .unwrap();
        let resp: axum::response::Response = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v: &axum::http::HeaderValue| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(ct, "image/png");
    }

    #[tokio::test]
    async fn control_valid_action_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/control",
            serde_json::json!({ "action": "home" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn control_unknown_action_returns_400() {
        let (status, body) = post_json(
            &test_router(),
            "/api/devices/TEST001/control",
            serde_json::json!({ "action": "unknown_key" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap_or("").contains("unknown_key"),
            "error must mention bad action: {body}"
        );
    }

    #[tokio::test]
    async fn tap_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/tap",
            serde_json::json!({ "x": 540.0, "y": 960.0 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn text_input_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/text",
            serde_json::json!({ "text": "hello world" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn app_start_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/apps/start",
            serde_json::json!({ "packageName": "com.example.app" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn app_start_empty_package_returns_422() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/apps/start",
            serde_json::json!({ "packageName": "" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn app_stop_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/apps/stop",
            serde_json::json!({ "packageName": "com.example.app" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn shell_returns_200() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/shell",
            serde_json::json!({ "command": "echo hi" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn shell_empty_command_returns_422() {
        let (status, _) = post_json(
            &test_router(),
            "/api/devices/TEST001/shell",
            serde_json::json!({ "command": "" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn scrcpy_balanced_returns_ok_and_preset() {
        let (status, body) = post_json(
            &test_router(),
            "/api/devices/TEST001/scrcpy",
            serde_json::json!({ "preset": "balanced" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["preset"], "balanced");
    }

    #[tokio::test]
    async fn scrcpy_latency_preset_echoed() {
        let (_, body) = post_json(
            &test_router(),
            "/api/devices/TEST001/scrcpy",
            serde_json::json!({ "preset": "latency" }),
        )
        .await;
        assert_eq!(body["preset"], "latency");
    }

    #[tokio::test]
    async fn approved_devices_initially_empty() {
        let (status, body) = get_json(&test_router(), "/api/approved-devices").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["devices"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn approve_known_device_succeeds() {
        let (status, body) = post_json(
            &test_router(),
            "/api/approved-devices/TEST001/approve",
            serde_json::json!({ "alias": "My Phone", "notes": "dev device" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["ok"], true);
        let approved = &body["approvedDevice"];
        assert_eq!(approved["serial"], "TEST001");
        assert_eq!(approved["alias"], "My Phone");
        assert_eq!(approved["approvalStatus"], "approved");
    }

    #[tokio::test]
    async fn approve_missing_device_returns_400() {
        let (status, body) = post_json(
            &test_router(),
            "/api/approved-devices/MISSING/approve",
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"].as_str().unwrap_or("").contains("MISSING"),
            "error must contain serial: {body}"
        );
    }

    #[tokio::test]
    async fn tools_status_has_correct_shape() {
        let (status, body) = get_json(&test_router(), "/api/tools/status").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["status"]["adb"].is_object());
        assert!(body["status"]["scrcpy"].is_object());
        assert!(body["status"]["vendorRoot"].is_string());
    }

    #[tokio::test]
    async fn help_topics_returns_four_entries() {
        let (status, body) = get_json(&test_router(), "/api/help/topics").await;
        assert_eq!(status, StatusCode::OK);
        let topics = body["topics"].as_array().unwrap();
        assert_eq!(topics.len(), 4);
        assert_eq!(topics[0]["id"], "connect");
    }

    // -- unit tests for helpers --

    #[test]
    fn enrich_without_overlay_is_candidate() {
        let device = DeviceRecord {
            serial: "S1".to_owned(),
            state: "device".to_owned(),
            model: None,
            product: None,
            device: None,
            transport_id: None,
        };
        let e = enrich(device, None);
        assert_eq!(e.approval_status, "candidate");
        assert!(!e.approved);
        assert!(e.alias.is_none());
    }

    #[test]
    fn enrich_with_approved_overlay() {
        let device = DeviceRecord {
            serial: "S1".to_owned(),
            state: "device".to_owned(),
            model: None,
            product: None,
            device: None,
            transport_id: None,
        };
        let overlay = RegistryMapEntry {
            serial: "S1".to_owned(),
            alias: Some("My Phone".to_owned()),
            approval_status: "approved".to_owned(),
            notes: None,
            first_seen_at: "2025-01-01T00:00:00.000Z".to_owned(),
            last_seen_at: "2025-01-02T00:00:00.000Z".to_owned(),
        };
        let e = enrich(device, Some(&overlay));
        assert!(e.approved);
        assert_eq!(e.alias.as_deref(), Some("My Phone"));
    }

    #[test]
    fn server_error_device_not_found_discriminant() {
        let e = ServerError::DeviceNotFound {
            serial: "X1".to_owned(),
        };
        assert!(e.is_device_not_found());
        assert!(!e.is_unsupported_action());
    }

    #[test]
    fn server_error_unsupported_action_discriminant() {
        let e = ServerError::Adb(AdbError::UnknownAction {
            action: "fly".to_owned(),
        });
        assert!(e.is_unsupported_action());
        assert!(!e.is_device_not_found());
    }

    #[test]
    fn wire_tool_status_from_tool_status() {
        use crate::tooling::ToolInfo;
        let ts = ToolStatus {
            vendor_root: PathBuf::from("/vendor"),
            adb: ToolInfo {
                installed: true,
                path: Some(PathBuf::from("/vendor/adb.exe")),
                version: Some("34.0.5".to_owned()),
            },
            scrcpy: ToolInfo {
                installed: false,
                path: None,
                version: None,
            },
        };
        let wire = WireToolStatus::from(ts);
        assert_eq!(wire.vendor_root, "/vendor");
        assert!(wire.adb.installed);
        assert_eq!(wire.adb.version.as_deref(), Some("34.0.5"));
        assert!(!wire.scrcpy.installed);
    }

    #[test]
    fn adb_to_registry_device_preserves_all_fields() {
        let adb_dev = DeviceRecord {
            serial: "ABC123".to_owned(),
            state: "device".to_owned(),
            model: Some("Pixel".to_owned()),
            product: Some("mustang".to_owned()),
            device: Some("mustang".to_owned()),
            transport_id: Some("3".to_owned()),
        };
        let reg_dev = adb_to_registry_device(&adb_dev);
        assert_eq!(reg_dev.serial, adb_dev.serial);
        assert_eq!(reg_dev.state, adb_dev.state);
        assert_eq!(reg_dev.model, adb_dev.model);
        assert_eq!(reg_dev.product, adb_dev.product);
        assert_eq!(reg_dev.device, adb_dev.device);
        assert_eq!(reg_dev.transport_id, adb_dev.transport_id);
    }
}
