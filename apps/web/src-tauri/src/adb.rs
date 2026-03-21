// DEPRECATED: apps/server/src/adb.ts — replaced by this native Rust ADB module
//
// ADB device operations for GlassBridge.
//
// All device I/O goes through [`AdbProcessBackend`], which shells out to the
// vendored `adb.exe`.  The [`DeviceOps`] trait defines the full interface so a
// mock or native-ADB backend can be swapped in later without touching callers.
//
// Key behavioural parity with the Node.js implementation:
//   - `list_devices` parses `adb devices -l` whitespace-tokenisation (not tab).
//   - `model`, `product`, `device` metadata fields have underscores replaced
//     with spaces (e.g. `Pixel_10_XL` → `Pixel 10 XL`).
//   - `list_packages` strips the `package:` prefix and sorts alphabetically.
//   - `input_text` replaces ASCII spaces with `%s` before passing to adb.
//   - `capture_screenshot` uses `exec-out screencap -p` and returns raw bytes.
//   - `start_scrcpy` spawns scrcpy detached (fire-and-forget).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::tooling::{self, ToolError};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by ADB device operations.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::adb::AdbError;
///
/// let e = AdbError::UnknownAction { action: "fly".into() };
/// assert!(e.to_string().contains("fly"));
/// ```
#[derive(Debug, Error)]
pub enum AdbError {
    /// The named key action has no mapping in [`KEY_MAP`].
    #[error("unsupported key action: '{action}'")]
    UnknownAction { action: String },

    /// A required tool (adb or scrcpy) could not be located.
    #[error("tool not found: {0}")]
    ToolNotFound(#[from] ToolError),

    /// The adb process could not be spawned or its output could not be read.
    #[error("adb process error: {0}")]
    Process(String),

    /// adb returned a non-zero exit code.
    #[error("adb exited with code {exit_code}: {stderr}")]
    ExitFailure { exit_code: i32, stderr: String },
}

#[allow(dead_code)]
impl AdbError {
    /// Returns `true` if this error represents a missing tool.
    pub fn is_tool_not_found(&self) -> bool {
        matches!(self, Self::ToolNotFound(_))
    }

    /// Returns `true` if this error is an unknown key action.
    pub fn is_unknown_action(&self) -> bool {
        matches!(self, Self::UnknownAction { .. })
    }
}

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// A single entry from `adb devices -l`.
///
/// `model`, `product`, `device`, and `transport_id` are `None` for devices
/// that report no metadata (e.g. unauthorized devices).
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::adb::DeviceRecord;
///
/// let rec = DeviceRecord {
///     serial: "19281FDEE000GS".into(),
///     state: "device".into(),
///     model: Some("Pixel 10 XL".into()),
///     product: Some("mustang".into()),
///     device: Some("mustang".into()),
///     transport_id: Some("1".into()),
/// };
/// assert_eq!(rec.model.as_deref(), Some("Pixel 10 XL"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    /// USB serial / emulator port string.
    pub serial: String,
    /// Connection state: `"device"`, `"unauthorized"`, `"offline"`, etc.
    pub state: String,
    /// Human-readable model name (underscores replaced with spaces).
    pub model: Option<String>,
    /// Product codename (underscores replaced with spaces).
    pub product: Option<String>,
    /// Device codename (underscores replaced with spaces).
    pub device: Option<String>,
    /// ADB transport identifier.
    pub transport_id: Option<String>,
}

/// Result of running an adb shell command.
///
/// Exit codes are passed through verbatim; callers decide whether a non-zero
/// code is an error.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    /// UTF-8 decoded standard output.
    pub stdout: String,
    /// UTF-8 decoded standard error.
    pub stderr: String,
    /// Process exit code (`-1` if the process was killed by a signal).
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
}

/// scrcpy quality preset, mirroring the TypeScript `"latency" | "balanced" | "quality"` union.
///
/// | Preset     | Bit-rate | Max FPS | Max size | Audio |
/// |------------|----------|---------|----------|-------|
/// | `Latency`  | 8 Mbps   | 60      | 1280 px  | off   |
/// | `Balanced` | 12 Mbps  | 60      | 1600 px  | on    |
/// | `Quality`  | 24 Mbps  | 60      | 2160 px  | on    |
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrcpyPreset {
    Latency,
    Balanced,
    Quality,
}

// ---------------------------------------------------------------------------
// DeviceOps trait
// ---------------------------------------------------------------------------

/// Async interface for ADB device operations.
///
/// [`AdbProcessBackend`] is the production implementation.  The trait exists
/// so tests and future native-ADB backends can substitute an alternative.
///
/// The `#[async_trait]` macro rewrites each `async fn` into a method returning
/// `Pin<Box<dyn Future + Send>>`, making the trait dyn-compatible so it can be
/// used as `Arc<dyn DeviceOps + Send + Sync>`.
#[async_trait]
pub trait DeviceOps: Send + Sync {
    /// Return all devices reported by `adb devices -l`.
    async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AdbError>;

    /// Return sorted third-party package names installed on `serial`.
    async fn list_packages(&self, serial: &str) -> Result<Vec<String>, AdbError>;

    /// Send a named key event to `serial`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError::UnknownAction`] if `action` has no mapping.
    async fn send_key(&self, serial: &str, action: &str) -> Result<CommandResult, AdbError>;

    /// Simulate a touch tap at `(x, y)` on `serial`.
    async fn tap(&self, serial: &str, x: f64, y: f64) -> Result<CommandResult, AdbError>;

    /// Type `text` on `serial` (spaces are sent as `%s`).
    async fn input_text(&self, serial: &str, text: &str) -> Result<CommandResult, AdbError>;

    /// Launch the default launcher activity for `package` on `serial`.
    async fn launch_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError>;

    /// Force-stop `package` on `serial`.
    async fn stop_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError>;

    /// Run an arbitrary shell `command` on `serial`.
    async fn shell_command(&self, serial: &str, command: &str) -> Result<CommandResult, AdbError>;

    /// Capture a PNG screenshot from `serial`, returning raw bytes.
    async fn capture_screenshot(&self, serial: &str) -> Result<Vec<u8>, AdbError>;

    /// Spawn scrcpy in a detached process for `serial` using `preset`.
    async fn start_scrcpy(&self, serial: &str, preset: ScrcpyPreset) -> Result<(), AdbError>;
}

// ---------------------------------------------------------------------------
// Key map
// ---------------------------------------------------------------------------

/// Maps GlassBridge action names to Android keycodes.
static KEY_MAP: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| {
        HashMap::from([
            ("home", "KEYCODE_HOME"),
            ("back", "KEYCODE_BACK"),
            ("recents", "KEYCODE_APP_SWITCH"),
            ("power", "KEYCODE_POWER"),
            ("wake", "KEYCODE_WAKEUP"),
            ("volumeUp", "KEYCODE_VOLUME_UP"),
            ("volumeDown", "KEYCODE_VOLUME_DOWN"),
        ])
    });

// ---------------------------------------------------------------------------
// AdbProcessBackend
// ---------------------------------------------------------------------------

/// Production [`DeviceOps`] implementation that shells out to the vendored
/// `adb.exe` (and `scrcpy.exe`) via `tokio::process::Command`.
///
/// Construct with [`AdbProcessBackend::new`] and share via `Arc<dyn DeviceOps>`.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::adb::{AdbProcessBackend, DeviceOps};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let backend = AdbProcessBackend::new(Path::new("/repo/vendor"));
/// let devices = backend.list_devices().await?;
/// println!("found {} device(s)", devices.len());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct AdbProcessBackend {
    vendor_root: PathBuf,
}

impl AdbProcessBackend {
    /// Create a new backend that resolves tools relative to `vendor_root`.
    pub fn new(vendor_root: &Path) -> Self {
        Self {
            vendor_root: vendor_root.to_path_buf(),
        }
    }

    // -- private helpers ---------------------------------------------------

    async fn adb_path(&self) -> Result<PathBuf, AdbError> {
        tooling::get_adb_path(&self.vendor_root)
            .await
            .map_err(AdbError::ToolNotFound)
    }

    async fn scrcpy_path(&self) -> Result<PathBuf, AdbError> {
        tooling::get_scrcpy_path(&self.vendor_root)
            .await
            .map_err(AdbError::ToolNotFound)
    }

    /// Run `adb -s <serial> <args...>` and return a [`CommandResult`].
    async fn run_adb(&self, serial: &str, args: &[&str]) -> Result<CommandResult, AdbError> {
        let adb = self.adb_path().await?;
        let mut full_args = vec!["-s", serial];
        full_args.extend_from_slice(args);

        let out = tooling::run_command(&adb, &full_args)
            .await
            .map_err(|e| AdbError::Process(e.to_string()))?;

        Ok(CommandResult {
            stdout: out.stdout,
            stderr: out.stderr,
            exit_code: out.exit_code,
        })
    }
}

// ---------------------------------------------------------------------------
// DeviceOps implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl DeviceOps for AdbProcessBackend {
    async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AdbError> {
        let adb = self.adb_path().await?;
        let out = tooling::run_command(&adb, &["devices", "-l"])
            .await
            .map_err(|e| AdbError::Process(e.to_string()))?;

        let devices = out
            .stdout
            .lines()
            .skip(1) // skip "List of devices attached"
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(parse_device_line)
            .collect();

        Ok(devices)
    }

    async fn list_packages(&self, serial: &str) -> Result<Vec<String>, AdbError> {
        let result = self
            .run_adb(serial, &["shell", "pm", "list", "packages", "-3"])
            .await?;

        let mut packages: Vec<String> = result
            .stdout
            .lines()
            .map(|line| line.trim().trim_start_matches("package:").to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        packages.sort_unstable();
        Ok(packages)
    }

    async fn send_key(&self, serial: &str, action: &str) -> Result<CommandResult, AdbError> {
        let keycode = KEY_MAP.get(action).ok_or_else(|| AdbError::UnknownAction {
            action: action.to_owned(),
        })?;

        self.run_adb(serial, &["shell", "input", "keyevent", keycode])
            .await
    }

    async fn tap(&self, serial: &str, x: f64, y: f64) -> Result<CommandResult, AdbError> {
        // Format as integers when the values have no fractional part to match
        // the adb input tap spec (integer pixels).
        let xs = format_coord(x);
        let ys = format_coord(y);
        self.run_adb(serial, &["shell", "input", "tap", &xs, &ys])
            .await
    }

    async fn input_text(&self, serial: &str, text: &str) -> Result<CommandResult, AdbError> {
        // adb shell input text treats spaces as argument separators; escape
        // them as %s, matching the Node.js implementation.
        let escaped = text.replace(' ', "%s");
        self.run_adb(serial, &["shell", "input", "text", &escaped])
            .await
    }

    async fn launch_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError> {
        self.run_adb(
            serial,
            &[
                "shell",
                "monkey",
                "-p",
                package,
                "-c",
                "android.intent.category.LAUNCHER",
                "1",
            ],
        )
        .await
    }

    async fn stop_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError> {
        self.run_adb(serial, &["shell", "am", "force-stop", package])
            .await
    }

    async fn shell_command(&self, serial: &str, command: &str) -> Result<CommandResult, AdbError> {
        self.run_adb(serial, &["shell", command]).await
    }

    async fn capture_screenshot(&self, serial: &str) -> Result<Vec<u8>, AdbError> {
        let adb = self.adb_path().await?;
        let (bytes, exit_code) =
            tooling::run_command_buffer(&adb, &["-s", serial, "exec-out", "screencap", "-p"])
                .await
                .map_err(|e| AdbError::Process(e.to_string()))?;

        if exit_code != 0 {
            return Err(AdbError::ExitFailure {
                exit_code,
                stderr: "(binary capture — stderr not collected)".to_owned(),
            });
        }

        Ok(bytes)
    }

    async fn start_scrcpy(&self, serial: &str, preset: ScrcpyPreset) -> Result<(), AdbError> {
        let scrcpy = self.scrcpy_path().await?;

        let mut args: Vec<std::ffi::OsString> = vec!["-s".into(), serial.into()];

        match preset {
            ScrcpyPreset::Latency => {
                args.push("--video-bit-rate=8M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=1280".into());
                args.push("--no-audio".into());
            }
            ScrcpyPreset::Quality => {
                args.push("--video-bit-rate=24M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=2160".into());
            }
            ScrcpyPreset::Balanced => {
                args.push("--video-bit-rate=12M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=1600".into());
            }
        }

        // Detached: spawn and immediately forget — do not await exit.
        let mut child = tokio::process::Command::new(&scrcpy)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| AdbError::Process(format!("failed to spawn scrcpy: {e}")))?;

        // Detach — equivalent to Node's `child.unref()`.
        // We drop the Child handle so Tokio does not wait for it.
        // A background task reaps the process to avoid zombies.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse one line from `adb devices -l` into a [`DeviceRecord`].
///
/// The line format is whitespace-separated tokens:
/// ```text
/// <serial>  <state>  [key:value ...]
/// ```
/// where common key names are `model`, `product`, `device`, `transport_id`,
/// `usb`, and `product`.
///
/// Underscores in `model`, `product`, and `device` values are replaced with
/// spaces, matching the Node.js `parseDeviceLine` function.
fn parse_device_line(line: &str) -> DeviceRecord {
    let mut tokens = line.split_whitespace();
    let serial = tokens.next().unwrap_or("").to_owned();
    let state = tokens.next().unwrap_or("").to_owned();

    let mut meta: HashMap<&str, &str> = HashMap::new();
    for token in tokens {
        if let Some((k, v)) = token.split_once(':') {
            meta.insert(k, v);
        }
    }

    DeviceRecord {
        serial,
        state,
        model: meta.get("model").map(|v| v.replace('_', " ")),
        product: meta.get("product").map(|v| v.replace('_', " ")),
        device: meta.get("device").map(|v| v.replace('_', " ")),
        transport_id: meta.get("transport_id").map(|v| (*v).to_owned()),
    }
}

/// Format a float coordinate for `adb input tap`.
///
/// Drops the decimal part when the value is a whole number (matches JS
/// `String(x)` which also omits `.0`).
fn format_coord(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb_mock::{
        DEVICES_RESPONSE_EMPTY, DEVICES_RESPONSE_ONE_PIXEL, DEVICES_RESPONSE_TWO,
        DEVICES_RESPONSE_UNAUTHORIZED,
    };

    // -- parse_device_line ---------------------------------------------------

    #[test]
    fn parses_full_pixel_line() {
        // Tab-separated as adb emits it; parse_device_line uses split_whitespace
        // so tabs and spaces both work.
        let line = "19281FDEE000GS\t device transport_id:1 usb:1-1 product:mustang model:Pixel_10_XL device:mustang";
        let rec = parse_device_line(line);
        assert_eq!(rec.serial, "19281FDEE000GS");
        assert_eq!(rec.state, "device");
        assert_eq!(rec.model.as_deref(), Some("Pixel 10 XL"));
        assert_eq!(rec.product.as_deref(), Some("mustang"));
        assert_eq!(rec.device.as_deref(), Some("mustang"));
        assert_eq!(rec.transport_id.as_deref(), Some("1"));
    }

    #[test]
    fn parses_unauthorized_line_has_no_metadata() {
        let line = "19281FDEE000GS\t unauthorized transport_id:1 usb:1-1";
        let rec = parse_device_line(line);
        assert_eq!(rec.serial, "19281FDEE000GS");
        assert_eq!(rec.state, "unauthorized");
        assert!(rec.model.is_none());
        assert!(rec.product.is_none());
        assert!(rec.device.is_none());
        // transport_id is present even for unauthorized
        assert_eq!(rec.transport_id.as_deref(), Some("1"));
    }

    #[test]
    fn underscore_replaced_with_space_in_model() {
        let line = "ABC123 device model:Galaxy_S25_Ultra product:dream3qltedcm device:dream3q transport_id:3";
        let rec = parse_device_line(line);
        assert_eq!(rec.model.as_deref(), Some("Galaxy S25 Ultra"));
        assert_eq!(rec.product.as_deref(), Some("dream3qltedcm"));
        assert_eq!(rec.device.as_deref(), Some("dream3q"));
    }

    #[test]
    fn empty_line_yields_empty_serial_and_state() {
        let rec = parse_device_line("");
        assert_eq!(rec.serial, "");
        assert_eq!(rec.state, "");
        assert!(rec.model.is_none());
    }

    // -- list_devices parsing via fixtures -----------------------------------

    fn parse_devices_output(output: &str) -> Vec<DeviceRecord> {
        output
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(parse_device_line)
            .collect()
    }

    #[test]
    fn fixture_one_pixel_parses_correctly() {
        let devices = parse_devices_output(DEVICES_RESPONSE_ONE_PIXEL);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].serial, "19281FDEE000GS");
        assert_eq!(devices[0].state, "device");
        assert_eq!(devices[0].model.as_deref(), Some("Pixel 10 XL"));
    }

    #[test]
    fn fixture_empty_parses_to_empty_vec() {
        let devices = parse_devices_output(DEVICES_RESPONSE_EMPTY);
        assert!(devices.is_empty());
    }

    #[test]
    fn fixture_unauthorized_parses_state_correctly() {
        let devices = parse_devices_output(DEVICES_RESPONSE_UNAUTHORIZED);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].state, "unauthorized");
        assert!(devices[0].model.is_none());
    }

    #[test]
    fn fixture_two_devices_parses_both() {
        let devices = parse_devices_output(DEVICES_RESPONSE_TWO);
        assert_eq!(devices.len(), 2);
        let serials: Vec<&str> = devices.iter().map(|d| d.serial.as_str()).collect();
        assert!(serials.contains(&"19281FDEE000GS"));
        assert!(serials.contains(&"emulator-5554"));
    }

    // -- format_coord --------------------------------------------------------

    #[test]
    fn format_coord_whole_number_has_no_decimal() {
        assert_eq!(format_coord(100.0), "100");
        assert_eq!(format_coord(0.0), "0");
        assert_eq!(format_coord(1920.0), "1920");
    }

    #[test]
    fn format_coord_fractional_preserved() {
        assert_eq!(format_coord(100.5), "100.5");
    }

    // -- key map -------------------------------------------------------------

    #[test]
    fn all_documented_actions_are_mapped() {
        for action in &[
            "home",
            "back",
            "recents",
            "power",
            "wake",
            "volumeUp",
            "volumeDown",
        ] {
            assert!(
                KEY_MAP.contains_key(*action),
                "keyMap missing action '{action}'"
            );
        }
    }

    #[test]
    fn unknown_action_produces_correct_error() {
        // We need an async context only to drive the future; we test the mapping
        // synchronously via KEY_MAP directly.
        assert!(KEY_MAP.get("fly").is_none());
    }

    // -- serialisation -------------------------------------------------------

    #[test]
    fn device_record_serialises_camel_case() {
        let rec = DeviceRecord {
            serial: "ABC".into(),
            state: "device".into(),
            model: Some("Pixel 10 XL".into()),
            product: None,
            device: None,
            transport_id: Some("1".into()),
        };
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("serial").is_some());
        assert!(json.get("state").is_some());
        assert!(json.get("model").is_some());
        assert!(
            json.get("transportId").is_some(),
            "must be camelCase transportId"
        );
        assert!(
            json.get("transport_id").is_none(),
            "must NOT have snake_case key"
        );
    }

    #[test]
    fn command_result_serialises_exit_code_camel_case() {
        let res = CommandResult {
            stdout: "ok".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert!(json.get("exitCode").is_some(), "must be exitCode");
        assert!(json.get("exit_code").is_none());
    }

    // -- AdbError helpers ----------------------------------------------------

    #[test]
    fn is_unknown_action_distinguishes_variants() {
        let e = AdbError::UnknownAction { action: "x".into() };
        assert!(e.is_unknown_action());
        assert!(!e.is_tool_not_found());
    }
}
