// adb_native.rs — Phase 2 native ADB backend for GlassBridge.
//
// [`AdbNativeBackend`] implements [`DeviceOps`] by talking directly to the ADB
// server daemon over TCP (default 127.0.0.1:5037) via the `adb_client` crate,
// eliminating all `adb.exe` process spawning for device commands.
//
// The `adb_client` crate is entirely synchronous (blocking I/O on top of
// `std::net::TcpStream`).  Every public method bridges into async by
// dispatching the blocking work onto `tokio::task::spawn_blocking`.
//
// # Feature gate
//
// This entire module is compiled only when the `native-adb` Cargo feature is
// enabled (default on).  The factory function [`super::adb::create_adb_backend`]
// tries to construct this backend first and falls back to
// [`AdbProcessBackend`] on failure.
//
// # Thread safety
//
// `AdbNativeBackend` is `Send + Sync` — it holds only a `SocketAddrV4`, which
// is `Copy`.  A fresh `ADBServer` / `ADBServerDevice` is created per operation
// inside each `spawn_blocking` closure; the `adb_client` types are never
// shared across tasks.

use std::net::{Ipv4Addr, SocketAddrV4};

use adb_client::server::ADBServer;
use adb_client::server::DeviceLong;
use adb_client::server_device::ADBServerDevice;
use adb_client::ADBDeviceExt as _;
use async_trait::async_trait;

use crate::adb::{AdbError, CommandResult, DeviceOps, DeviceRecord, ScrcpyPreset, KEY_MAP};

// ---------------------------------------------------------------------------
// Additional AdbError variants for the native backend
// ---------------------------------------------------------------------------

/// Wraps an [`adb_client::RustADBError`] as an [`AdbError::Process`] string.
fn native_err(e: adb_client::RustADBError) -> AdbError {
    AdbError::Process(e.to_string())
}

/// Converts a `tokio::task::JoinError` (task panic / cancellation) into
/// [`AdbError::Process`].
fn join_err(e: tokio::task::JoinError) -> AdbError {
    AdbError::Process(format!("spawn_blocking task failed: {e}"))
}

// ---------------------------------------------------------------------------
// AdbNativeBackend
// ---------------------------------------------------------------------------

/// [`DeviceOps`] implementation that communicates with the ADB server daemon
/// directly over TCP using the `adb_client` crate.
///
/// A new TCP connection to the ADB server is opened for every operation.
/// This is intentional: `adb_client`'s transport is stateful and not
/// `Clone`, so connection pooling would require a `Mutex<ADBServer>` that
/// serialises all I/O.  The per-call overhead is negligible because the ADB
/// server itself runs locally (127.0.0.1:5037).
///
/// # Examples
///
/// ```rust,no_run
/// use glassbridge_desktop::adb_native::AdbNativeBackend;
/// use glassbridge_desktop::adb::DeviceOps;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let backend = AdbNativeBackend::new()?;
/// let devices = backend.list_devices().await?;
/// println!("found {} device(s)", devices.len());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct AdbNativeBackend {
    server_addr: SocketAddrV4,
}

impl AdbNativeBackend {
    /// Default ADB server port.
    pub const DEFAULT_PORT: u16 = 5037;

    /// Construct a backend that connects to `127.0.0.1:5037`.
    ///
    /// Returns `Ok` immediately — the TCP connection is not opened until the
    /// first operation is performed.
    ///
    /// # Errors
    ///
    /// Currently infallible, but returns `Result` so callers do not need to
    /// change their signature if health-check logic is added in the future.
    pub fn new() -> Result<Self, AdbError> {
        Ok(Self {
            server_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, Self::DEFAULT_PORT),
        })
    }

    /// Construct a backend that connects to a custom `server_addr`.
    ///
    /// Useful for tests that spin up a mock ADB server on an arbitrary port.
    #[must_use]
    pub fn with_addr(server_addr: SocketAddrV4) -> Self {
        Self { server_addr }
    }

    // -- Private helpers -----------------------------------------------------

    /// Run a shell command on `serial`, collecting stdout and stderr into
    /// `Vec<u8>` buffers.  Returns `(stdout_bytes, stderr_bytes, exit_code)`.
    ///
    /// The exit code is `0` when the shell exits cleanly or `None` is returned
    /// from `shell_command` (connection closed without an explicit exit
    /// packet).
    fn run_shell_blocking(
        server_addr: SocketAddrV4,
        serial: String,
        cmd: String,
    ) -> Result<(Vec<u8>, Vec<u8>, i32), adb_client::RustADBError> {
        let mut device = ADBServerDevice::new(serial, Some(server_addr));
        let mut stdout_buf: Vec<u8> = Vec::new();
        let mut stderr_buf: Vec<u8> = Vec::new();

        let exit_code = device.shell_command(&cmd, Some(&mut stdout_buf), Some(&mut stderr_buf))?;

        Ok((
            stdout_buf,
            stderr_buf,
            exit_code.map(i32::from).unwrap_or(0),
        ))
    }

    /// Convenience: run a shell command and decode stdout as UTF-8, returning
    /// a [`CommandResult`].
    async fn shell(
        &self,
        serial: &str,
        cmd: impl Into<String> + Send + 'static,
    ) -> Result<CommandResult, AdbError> {
        let addr = self.server_addr;
        let serial = serial.to_owned();
        let cmd = cmd.into();

        let (out, err, code) =
            tokio::task::spawn_blocking(move || Self::run_shell_blocking(addr, serial, cmd))
                .await
                .map_err(join_err)?
                .map_err(native_err)?;

        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            exit_code: code,
        })
    }
}

// ---------------------------------------------------------------------------
// DeviceRecord conversion
// ---------------------------------------------------------------------------

/// Convert a [`DeviceLong`] from `adb_client` into our [`DeviceRecord`].
///
/// `model`, `product`, and `device` carry underscores in the raw ADB output;
/// we normalise them to spaces to match the process-backend behaviour.
/// "Unk" is the sentinel the crate inserts when a field is absent — we
/// translate that to `None`.
fn device_long_to_record(d: DeviceLong) -> DeviceRecord {
    fn opt_field(s: String) -> Option<String> {
        if s.is_empty() || s == "Unk" {
            None
        } else {
            Some(s.replace('_', " "))
        }
    }

    DeviceRecord {
        serial: d.identifier,
        state: d.state.to_string(),
        model: opt_field(d.model),
        product: opt_field(d.product),
        device: opt_field(d.device),
        transport_id: Some(d.transport_id.to_string()),
    }
}

// ---------------------------------------------------------------------------
// DeviceOps implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl DeviceOps for AdbNativeBackend {
    /// Return all devices reported by the ADB server.
    ///
    /// Uses `devices_long` so model/product/device metadata is available
    /// without separate `getprop` round-trips.
    async fn list_devices(&self) -> Result<Vec<DeviceRecord>, AdbError> {
        let addr = self.server_addr;

        let devices_long: Vec<DeviceLong> = tokio::task::spawn_blocking(move || {
            // ADBServer is constructed entirely inside the blocking task so it
            // never crosses an await point — it is not required to be Send.
            ADBServer::new(addr).devices_long()
        })
        .await
        .map_err(join_err)?
        .map_err(native_err)?;

        Ok(devices_long
            .into_iter()
            .map(device_long_to_record)
            .collect())
    }

    /// Return sorted third-party package names installed on `serial`.
    ///
    /// Equivalent to `adb -s <serial> shell pm list packages -3`.
    async fn list_packages(&self, serial: &str) -> Result<Vec<String>, AdbError> {
        let result = self.shell(serial, "pm list packages -3").await?;

        let mut packages: Vec<String> = result
            .stdout
            .lines()
            .map(|line| line.trim().trim_start_matches("package:").to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        packages.sort_unstable();
        Ok(packages)
    }

    /// Send a named key event to `serial`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError::UnknownAction`] when `action` has no entry in
    /// [`KEY_MAP`].
    async fn send_key(&self, serial: &str, action: &str) -> Result<CommandResult, AdbError> {
        let keycode = KEY_MAP.get(action).ok_or_else(|| AdbError::UnknownAction {
            action: action.to_owned(),
        })?;

        self.shell(serial, format!("input keyevent {keycode}"))
            .await
    }

    /// Simulate a touch tap at pixel coordinates `(x, y)`.
    async fn tap(&self, serial: &str, x: f64, y: f64) -> Result<CommandResult, AdbError> {
        let xs = format_coord(x);
        let ys = format_coord(y);
        self.shell(serial, format!("input tap {xs} {ys}")).await
    }

    /// Type `text` on `serial`.
    ///
    /// Spaces are transmitted as `%s` to prevent the shell from splitting the
    /// argument, matching the process-backend and Node.js behaviour.
    async fn input_text(&self, serial: &str, text: &str) -> Result<CommandResult, AdbError> {
        let escaped = text.replace(' ', "%s");
        self.shell(serial, format!("input text {escaped}")).await
    }

    /// Launch the default launcher activity for `package`.
    async fn launch_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError> {
        self.shell(
            serial,
            format!("monkey -p {package} -c android.intent.category.LAUNCHER 1"),
        )
        .await
    }

    /// Force-stop `package`.
    async fn stop_package(&self, serial: &str, package: &str) -> Result<CommandResult, AdbError> {
        self.shell(serial, format!("am force-stop {package}")).await
    }

    /// Run an arbitrary shell command on `serial`.
    async fn shell_command(&self, serial: &str, command: &str) -> Result<CommandResult, AdbError> {
        self.shell(serial, command.to_owned()).await
    }

    /// Capture a PNG screenshot from `serial` and return the raw bytes.
    ///
    /// This method tries the `adb_client` framebuffer protocol first.  That
    /// protocol requires the `framebuffer` service (available on most physical
    /// devices) and produces a proper RGBA image which the crate converts to
    /// PNG internally.  If that fails (e.g. the service is not available on
    /// the target device), we fall back to `screencap -p` over a shell
    /// command and stream the bytes back through a `Vec<u8>`.
    async fn capture_screenshot(&self, serial: &str) -> Result<Vec<u8>, AdbError> {
        let addr = self.server_addr;
        let serial_owned = serial.to_owned();

        // Attempt 1: framebuffer_bytes() — native protocol, returns PNG.
        let framebuffer_result: Result<Vec<u8>, adb_client::RustADBError> =
            tokio::task::spawn_blocking({
                let serial_owned = serial_owned.clone();
                move || {
                    let mut device = ADBServerDevice::new(serial_owned, Some(addr));
                    device.framebuffer_bytes()
                }
            })
            .await
            .map_err(join_err)?;

        match framebuffer_result {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                tracing::debug!(
                    serial = %serial_owned,
                    error = %e,
                    "framebuffer protocol unavailable, falling back to screencap -p"
                );
            }
        }

        // Attempt 2: `screencap -p` shell command.
        let (raw_bytes, _stderr, exit_code) = tokio::task::spawn_blocking(move || {
            // screencap -p writes a PNG directly to stdout.
            // We use the exec() path via shell_command so we get the raw
            // bytes — shell_command writes stdout into a Vec<u8>.
            Self::run_shell_blocking(addr, serial_owned, "screencap -p".to_owned())
        })
        .await
        .map_err(join_err)?
        .map_err(native_err)?;

        if exit_code != 0 {
            return Err(AdbError::ExitFailure {
                exit_code,
                stderr: "(screencap -p — see device log)".to_owned(),
            });
        }

        Ok(raw_bytes)
    }

    /// Spawn scrcpy detached for `serial` using `preset`.
    ///
    /// scrcpy is not replaced until Phase 3; this method delegates to the
    /// same process-spawn logic used by [`AdbProcessBackend`].  The scrcpy
    /// path is resolved from the vendor manifest via [`crate::tooling`].
    async fn start_scrcpy(&self, serial: &str, preset: ScrcpyPreset) -> Result<(), AdbError> {
        // Resolve scrcpy path via the tooling module (vendor manifest + fallbacks).
        let scrcpy = crate::tooling::get_scrcpy_path(
            // scrcpy lives in the same vendor tree; we resolve relative to the
            // binary's parent directory, matching AdbProcessBackend.
            &std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("vendor")))
                .unwrap_or_else(|| std::path::PathBuf::from("vendor")),
        )
        .await
        .map_err(AdbError::ToolNotFound)?;

        let mut args: Vec<std::ffi::OsString> = vec!["-s".into(), serial.into()];

        match preset {
            ScrcpyPreset::Latency => {
                args.push("--video-bit-rate=8M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=1280".into());
                args.push("--no-audio".into());
            }
            ScrcpyPreset::Balanced => {
                args.push("--video-bit-rate=12M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=1600".into());
            }
            ScrcpyPreset::Quality => {
                args.push("--video-bit-rate=24M".into());
                args.push("--max-fps=60".into());
                args.push("--max-size=2160".into());
            }
        }

        let mut child = tokio::process::Command::new(&scrcpy)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| AdbError::Process(format!("failed to spawn scrcpy: {e}")))?;

        // Detach — let a background task reap the process to avoid zombies.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Coordinate formatting (shared with process backend)
// ---------------------------------------------------------------------------

/// Format a floating-point coordinate for `input tap`.
///
/// Drops the fractional part when the value is a whole number, matching
/// JavaScript's `String(x)` behaviour and the process-backend helper.
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

    // -- format_coord --------------------------------------------------------

    #[test]
    fn format_coord_whole_drops_decimal() {
        assert_eq!(format_coord(100.0), "100");
        assert_eq!(format_coord(0.0), "0");
        assert_eq!(format_coord(1920.0), "1920");
    }

    #[test]
    fn format_coord_fractional_kept() {
        assert_eq!(format_coord(100.5), "100.5");
        assert_eq!(format_coord(0.25), "0.25");
    }

    // -- device_long_to_record -----------------------------------------------

    fn make_device_long(
        identifier: &str,
        state_str: &str,
        model: &str,
        product: &str,
        device: &str,
        transport_id: u32,
    ) -> DeviceLong {
        use std::str::FromStr as _;
        DeviceLong {
            identifier: identifier.to_owned(),
            state: adb_client::server::DeviceState::from_str(state_str).unwrap(),
            usb: "1-1".to_owned(),
            model: model.to_owned(),
            product: product.to_owned(),
            device: device.to_owned(),
            transport_id,
        }
    }

    #[test]
    fn converts_model_underscores_to_spaces() {
        let long = make_device_long("ABC123", "device", "Pixel_10_XL", "mustang", "mustang", 1);
        let rec = device_long_to_record(long);
        assert_eq!(rec.model.as_deref(), Some("Pixel 10 XL"));
        assert_eq!(rec.product.as_deref(), Some("mustang"));
        assert_eq!(rec.device.as_deref(), Some("mustang"));
    }

    #[test]
    fn converts_unk_sentinel_to_none() {
        let long = make_device_long("ABC123", "device", "Unk", "Unk", "Unk", 1);
        let rec = device_long_to_record(long);
        assert!(rec.model.is_none(), "Unk sentinel should become None");
        assert!(rec.product.is_none());
        assert!(rec.device.is_none());
    }

    #[test]
    fn converts_empty_field_to_none() {
        let long = make_device_long("ABC123", "device", "", "", "", 2);
        let rec = device_long_to_record(long);
        assert!(rec.model.is_none(), "empty string should become None");
    }

    #[test]
    fn transport_id_is_stringified() {
        let long = make_device_long("serial1", "device", "Model", "product", "device", 42);
        let rec = device_long_to_record(long);
        assert_eq!(rec.transport_id.as_deref(), Some("42"));
    }

    #[test]
    fn state_is_lowercase_string() {
        let long = make_device_long("serial1", "unauthorized", "Unk", "Unk", "Unk", 1);
        let rec = device_long_to_record(long);
        assert_eq!(rec.state, "unauthorized");
    }

    #[test]
    fn serial_is_preserved_verbatim() {
        let long = make_device_long("19281FDEE000GS", "device", "Unk", "Unk", "Unk", 1);
        let rec = device_long_to_record(long);
        assert_eq!(rec.serial, "19281FDEE000GS");
    }

    // -- AdbNativeBackend constructor ----------------------------------------

    #[test]
    fn new_returns_ok_with_default_port() {
        let backend = AdbNativeBackend::new().expect("constructor must not fail");
        assert_eq!(backend.server_addr.port(), AdbNativeBackend::DEFAULT_PORT);
        assert!(backend.server_addr.ip().is_loopback());
    }

    #[test]
    fn with_addr_stores_custom_address() {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9999);
        let backend = AdbNativeBackend::with_addr(addr);
        assert_eq!(backend.server_addr, addr);
    }

    // -- Trait object compatibility ------------------------------------------

    #[test]
    fn backend_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AdbNativeBackend>();
    }

    // -- Package list parsing (no device required) ---------------------------

    #[test]
    fn package_list_parse_strips_prefix_and_sorts() {
        // Simulate what the shell command output looks like.
        let raw = "package:com.example.app\npackage:com.android.chrome\npackage:org.foo\n";
        let mut packages: Vec<String> = raw
            .lines()
            .map(|line| line.trim().trim_start_matches("package:").to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        packages.sort_unstable();
        assert_eq!(
            packages,
            vec!["com.android.chrome", "com.example.app", "org.foo"]
        );
    }

    // -- Key-map lookup (shared map, no device required) ---------------------

    #[test]
    fn known_actions_resolve_to_keycodes() {
        for (action, expected_keycode) in &[
            ("home", "KEYCODE_HOME"),
            ("back", "KEYCODE_BACK"),
            ("recents", "KEYCODE_APP_SWITCH"),
            ("power", "KEYCODE_POWER"),
            ("wake", "KEYCODE_WAKEUP"),
            ("volumeUp", "KEYCODE_VOLUME_UP"),
            ("volumeDown", "KEYCODE_VOLUME_DOWN"),
        ] {
            assert_eq!(
                KEY_MAP.get(*action).copied(),
                Some(*expected_keycode),
                "action '{action}' must map to '{expected_keycode}'"
            );
        }
    }

    #[test]
    fn unknown_action_returns_error_variant() {
        // Mirror the guard in send_key without needing an async context.
        let result = KEY_MAP.get("fly");
        assert!(result.is_none(), "unknown action must not be in KEY_MAP");
    }
}
