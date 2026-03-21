// wifi_adb.rs — WiFi ADB (Android wireless debugging) support for GlassBridge.
//
// Provides TCP/IP connection management for ADB wireless debugging, including
// Android 11+ pairing-code support.  All operations delegate to the vendored
// `adb.exe` via `tokio::process::Command`, exactly like `AdbProcessBackend` in
// `adb.rs`, so the ADB server handles the actual TCP connection bookkeeping.
//
// Key operations:
//   - `connect`        → adb connect <ip>:<port>
//   - `disconnect`     → adb disconnect <ip>:<port>
//   - `enable_tcpip`   → adb -s <serial> tcpip <port>  (USB → WiFi handoff)
//   - `pair`           → adb pair <ip>:<port> <code>    (Android 11+ pairing)
//   - `list_wifi_devices` → filter `adb devices -l` for IP-address serials
//
// Error handling: all methods return `Result<_, WifiAdbError>`.  Callers that
// treat WiFi as optional should use `if let Ok(...)` or `unwrap_or_default()`.
//
// Thread safety: `WifiAdb` is `Clone + Send + Sync`.  Share via `Arc<WifiAdb>`
// or embed in `AppState`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;
use tracing::debug;

use crate::tooling::{self, ToolError};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by WiFi ADB operations.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::wifi_adb::WifiAdbError;
///
/// let e = WifiAdbError::InvalidAddress { address: "notanip".into() };
/// assert!(e.to_string().contains("notanip"));
/// ```
#[derive(Debug, Error)]
pub enum WifiAdbError {
    /// The provided address string is not a valid `ip:port` form.
    #[error("invalid WiFi ADB address '{address}': expected ip:port")]
    InvalidAddress { address: String },

    /// The pairing code contains invalid characters (must be 6 ASCII digits).
    #[error("invalid pairing code '{code}': expected 6 ASCII digits")]
    InvalidPairingCode { code: String },

    /// The ADB tool could not be located.
    #[error("tool not found: {0}")]
    ToolNotFound(#[from] ToolError),

    /// The adb process could not be spawned or its output could not be read.
    #[error("adb process error: {0}")]
    Process(String),

    /// adb returned a non-zero exit code.
    #[error("adb exited with code {exit_code}: {stderr}")]
    ExitFailure { exit_code: i32, stderr: String },

    /// adb output indicated a connection failure (e.g. "error: " prefix).
    #[error("adb connection failed: {message}")]
    ConnectionFailed { message: String },
}

impl WifiAdbError {
    /// Returns `true` if the error is a validation error (bad address/code).
    pub fn is_validation(&self) -> bool {
        matches!(
            self,
            Self::InvalidAddress { .. } | Self::InvalidPairingCode { .. }
        )
    }

    /// Returns `true` if the error represents a tool-not-found failure.
    pub fn is_tool_not_found(&self) -> bool {
        matches!(self, Self::ToolNotFound(_))
    }
}

// ---------------------------------------------------------------------------
// WiFiDevice
// ---------------------------------------------------------------------------

/// A device connected to ADB over TCP/IP (WiFi or USB-tether).
///
/// The `address` field uses the canonical `ip:port` form that ADB uses as the
/// serial identifier for network-connected devices.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::wifi_adb::WifiDevice;
///
/// let d = WifiDevice {
///     address: "192.168.1.42:5555".into(),
///     serial: "192.168.1.42:5555".into(),
///     model: Some("Pixel 8".into()),
///     connected: true,
/// };
/// assert!(d.connected);
/// assert_eq!(d.address, "192.168.1.42:5555");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiDevice {
    /// TCP/IP address in `ip:port` form (same as ADB serial for network devices).
    pub address: String,
    /// ADB serial — for WiFi devices this equals `address`.
    pub serial: String,
    /// Human-readable model name if reported by ADB, with underscores replaced by spaces.
    pub model: Option<String>,
    /// `true` if the device is in the `"device"` state (authorised and online).
    pub connected: bool,
}

// ---------------------------------------------------------------------------
// WifiAdb
// ---------------------------------------------------------------------------

/// WiFi ADB operations backed by the vendored `adb.exe`.
///
/// All methods are `async` and safe to call from any Tokio task.
/// Construct with [`WifiAdb::new`] and share via `Arc<WifiAdb>`.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::wifi_adb::WifiAdb;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
/// let output = wifi.connect("192.168.1.42:5555").await?;
/// println!("{output}");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct WifiAdb {
    vendor_root: PathBuf,
}

impl WifiAdb {
    /// Create a new `WifiAdb` that resolves `adb.exe` relative to `vendor_root`.
    pub fn new(vendor_root: &Path) -> Self {
        Self {
            vendor_root: vendor_root.to_path_buf(),
        }
    }

    // -- Public API ----------------------------------------------------------

    /// Connect to a device over TCP/IP.
    ///
    /// Equivalent to `adb connect <address>`.  Returns the raw stdout from ADB
    /// on success (e.g. `"connected to 192.168.1.42:5555"`).
    ///
    /// # Errors
    ///
    /// - [`WifiAdbError::InvalidAddress`] if `address` is not `ip:port` form.
    /// - [`WifiAdbError::ConnectionFailed`] if ADB reports an error in its output.
    /// - [`WifiAdbError::ExitFailure`] if adb exits non-zero.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::path::Path;
    /// # use glassbridge_desktop::wifi_adb::WifiAdb;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
    /// let msg = wifi.connect("192.168.1.42:5555").await?;
    /// assert!(msg.contains("connected"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&self, address: &str) -> Result<String, WifiAdbError> {
        validate_address(address)?;
        debug!(wifi_adb.address = %address, "adb connect");
        let output = self.run_adb_global(&["connect", address]).await?;
        // adb connect can exit 0 but still print "error:" in stdout
        check_adb_output_for_error(&output, address)?;
        Ok(output.trim().to_owned())
    }

    /// Disconnect a WiFi-connected device.
    ///
    /// Equivalent to `adb disconnect <address>`.  Returns the raw stdout from ADB.
    /// Disconnecting a device that is not connected is not an error (ADB prints
    /// `"error: no such device"` but exits 0; this method returns that string).
    ///
    /// # Errors
    ///
    /// - [`WifiAdbError::InvalidAddress`] if `address` is not `ip:port` form.
    /// - [`WifiAdbError::ExitFailure`] if adb exits non-zero.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::path::Path;
    /// # use glassbridge_desktop::wifi_adb::WifiAdb;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
    /// let msg = wifi.disconnect("192.168.1.42:5555").await?;
    /// println!("{msg}");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn disconnect(&self, address: &str) -> Result<String, WifiAdbError> {
        validate_address(address)?;
        debug!(wifi_adb.address = %address, "adb disconnect");
        let output = self.run_adb_global(&["disconnect", address]).await?;
        Ok(output.trim().to_owned())
    }

    /// Switch a USB-connected device to TCP/IP mode on `port`.
    ///
    /// Equivalent to `adb -s <serial> tcpip <port>`.  After this succeeds,
    /// the device will accept connections on the given port.  The caller must
    /// then call [`connect`](Self::connect) with the device's IP address.
    ///
    /// The standard port for ADB-over-WiFi is `5555`.
    ///
    /// # Errors
    ///
    /// - [`WifiAdbError::ExitFailure`] if adb exits non-zero (device not found, etc.).
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::path::Path;
    /// # use glassbridge_desktop::wifi_adb::WifiAdb;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
    /// wifi.enable_tcpip("19281FDEE000GS", 5555).await?;
    /// // now call wifi.connect("192.168.x.x:5555")
    /// # Ok(())
    /// # }
    /// ```
    pub async fn enable_tcpip(&self, serial: &str, port: u16) -> Result<String, WifiAdbError> {
        debug!(
            wifi_adb.serial = %serial,
            wifi_adb.port = %port,
            "adb tcpip"
        );
        let port_str = port.to_string();
        let output = self
            .run_adb_with_serial(serial, &["tcpip", &port_str])
            .await?;
        Ok(output.trim().to_owned())
    }

    /// Pair with a device using Android 11+ wireless debugging pairing code.
    ///
    /// Equivalent to `adb pair <ip>:<port> <code>`.  The `address` must be the
    /// pairing port (shown in the Android "Pair device with pairing code" dialog),
    /// which is separate from the regular ADB port.  The `code` must be the
    /// 6-digit numeric code shown on the device.
    ///
    /// After pairing succeeds, call [`connect`](Self::connect) with the device's
    /// regular ADB port (usually 5555 or the port shown in wireless debugging
    /// settings).
    ///
    /// # Errors
    ///
    /// - [`WifiAdbError::InvalidAddress`] if `address` is not `ip:port` form.
    /// - [`WifiAdbError::InvalidPairingCode`] if `code` is not 6 ASCII digits.
    /// - [`WifiAdbError::ConnectionFailed`] if ADB reports an error in its output.
    /// - [`WifiAdbError::ExitFailure`] if adb exits non-zero.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::path::Path;
    /// # use glassbridge_desktop::wifi_adb::WifiAdb;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
    /// wifi.pair("192.168.1.42:37799", "123456").await?;
    /// // then connect on the regular ADB port:
    /// wifi.connect("192.168.1.42:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn pair(&self, address: &str, code: &str) -> Result<String, WifiAdbError> {
        validate_address(address)?;
        validate_pairing_code(code)?;
        debug!(
            wifi_adb.address = %address,
            "adb pair"
        );
        let output = self.run_adb_global(&["pair", address, code]).await?;
        check_adb_output_for_error(&output, address)?;
        Ok(output.trim().to_owned())
    }

    /// List all ADB devices that are connected over TCP/IP.
    ///
    /// Runs `adb devices -l` and filters for lines whose serial is in
    /// `ip:port` form.  USB-connected devices are excluded.
    ///
    /// # Errors
    ///
    /// - [`WifiAdbError::Process`] if adb cannot be spawned.
    /// - [`WifiAdbError::ExitFailure`] if adb exits non-zero.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::path::Path;
    /// # use glassbridge_desktop::wifi_adb::WifiAdb;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let wifi = WifiAdb::new(Path::new("/repo/vendor"));
    /// let devices = wifi.list_wifi_devices().await?;
    /// for d in &devices {
    ///     println!("{} — connected={}", d.address, d.connected);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list_wifi_devices(&self) -> Result<Vec<WifiDevice>, WifiAdbError> {
        let output = self.run_adb_global(&["devices", "-l"]).await?;
        let devices = parse_wifi_devices(&output);
        debug!(wifi_adb.count = %devices.len(), "list_wifi_devices");
        Ok(devices)
    }

    // -- Internal helpers ----------------------------------------------------

    /// Resolve the path to `adb.exe`.
    async fn adb_path(&self) -> Result<PathBuf, WifiAdbError> {
        tooling::get_adb_path(&self.vendor_root)
            .await
            .map_err(WifiAdbError::ToolNotFound)
    }

    /// Run an `adb` command without a `-s <serial>` qualifier (global commands
    /// such as `connect`, `disconnect`, `pair`, `devices`).
    async fn run_adb_global(&self, args: &[&str]) -> Result<String, WifiAdbError> {
        let adb = self.adb_path().await?;

        let out = Command::new(&adb)
            .args(args)
            .output()
            .await
            .map_err(|e| WifiAdbError::Process(format!("failed to spawn adb: {e}")))?;

        let exit_code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        if exit_code != 0 {
            return Err(WifiAdbError::ExitFailure { exit_code, stderr });
        }

        Ok(stdout)
    }

    /// Run `adb -s <serial> <args...>`.
    async fn run_adb_with_serial(
        &self,
        serial: &str,
        args: &[&str],
    ) -> Result<String, WifiAdbError> {
        let adb = self.adb_path().await?;

        let mut full_args = vec!["-s", serial];
        full_args.extend_from_slice(args);

        let out = Command::new(&adb)
            .args(&full_args)
            .output()
            .await
            .map_err(|e| WifiAdbError::Process(format!("failed to spawn adb: {e}")))?;

        let exit_code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

        if exit_code != 0 {
            return Err(WifiAdbError::ExitFailure { exit_code, stderr });
        }

        Ok(stdout)
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers (pub(crate) for unit test access)
// ---------------------------------------------------------------------------

/// Parse `adb devices -l` output and return only TCP/IP-connected devices.
///
/// A line represents a WiFi device when its first token (serial) contains a
/// colon (the `ip:port` delimiter used by ADB for network devices).
///
/// USB serials never contain colons, so this is unambiguous.
pub(crate) fn parse_wifi_devices(output: &str) -> Vec<WifiDevice> {
    output
        .lines()
        .skip(1) // skip "List of devices attached"
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(parse_wifi_device_line)
        .collect()
}

/// Parse a single `adb devices -l` line into a [`WifiDevice`] if it is a
/// TCP/IP device.  Returns `None` for USB or emulator serials.
fn parse_wifi_device_line(line: &str) -> Option<WifiDevice> {
    let mut tokens = line.split_whitespace();
    let serial = tokens.next()?;
    let state = tokens.next().unwrap_or("unknown");

    // WiFi serials contain ':' (ip:port); USB serials are alphanumeric only;
    // emulator serials look like "emulator-5554" (contain '-' but no ':').
    if !serial.contains(':') {
        return None;
    }

    // Parse key:value metadata tokens (model:, product:, etc.)
    let mut model: Option<String> = None;
    for token in tokens {
        if let Some(value) = token.strip_prefix("model:") {
            model = Some(value.replace('_', " "));
        }
    }

    Some(WifiDevice {
        address: serial.to_owned(),
        serial: serial.to_owned(),
        model,
        connected: state == "device",
    })
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate that `address` has the form `<host>:<port>` where port is 1–65535.
///
/// `<host>` may be an IPv4 address, IPv6 address (without brackets), or
/// hostname.  We accept any non-empty string for the host part to remain
/// permissive and not duplicate ADB's own validation.
fn validate_address(address: &str) -> Result<(), WifiAdbError> {
    // Must contain exactly one colon for the port separator (IPv4/hostname).
    // IPv6 addresses passed to adb use the form [::1]:port; we accept those
    // by checking the last colon.
    let Some(colon_pos) = address.rfind(':') else {
        return Err(WifiAdbError::InvalidAddress {
            address: address.to_owned(),
        });
    };

    let host = &address[..colon_pos];
    let port_str = &address[colon_pos + 1..];

    if host.is_empty() || port_str.is_empty() {
        return Err(WifiAdbError::InvalidAddress {
            address: address.to_owned(),
        });
    }

    // Port must be a valid u16 in range 1–65535.
    let port: u16 = port_str
        .parse()
        .ok()
        .filter(|&p: &u16| p > 0)
        .ok_or_else(|| WifiAdbError::InvalidAddress {
            address: address.to_owned(),
        })?;
    let _ = port; // used for validation only

    Ok(())
}

/// Validate that `code` is exactly 6 ASCII digits.
fn validate_pairing_code(code: &str) -> Result<(), WifiAdbError> {
    if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(WifiAdbError::InvalidPairingCode {
            code: code.to_owned(),
        })
    }
}

/// Inspect ADB stdout for a leading `"error:"` pattern that indicates a
/// connection-level failure (ADB exits 0 but the operation failed).
fn check_adb_output_for_error(output: &str, address: &str) -> Result<(), WifiAdbError> {
    let lower = output.to_ascii_lowercase();
    // adb prefixes connection errors with "error:" or "failed to"
    if lower.contains("error:") || lower.starts_with("failed to") {
        return Err(WifiAdbError::ConnectionFailed {
            message: format!("{} — {}", address, output.trim()),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_address ----------------------------------------------------

    #[test]
    fn valid_ipv4_port_passes() {
        assert!(validate_address("192.168.1.42:5555").is_ok());
        assert!(validate_address("10.0.0.1:5555").is_ok());
        assert!(validate_address("127.0.0.1:37799").is_ok());
    }

    #[test]
    fn valid_hostname_port_passes() {
        assert!(validate_address("android-device.local:5555").is_ok());
    }

    #[test]
    fn valid_ipv6_bracket_form_passes() {
        // ADB accepts [::1]:5555; rfind(':') picks the last colon before port.
        assert!(validate_address("[::1]:5555").is_ok());
    }

    #[test]
    fn missing_port_fails() {
        assert!(validate_address("192.168.1.42").is_err());
        assert!(validate_address("").is_err());
    }

    #[test]
    fn empty_host_fails() {
        assert!(validate_address(":5555").is_err());
    }

    #[test]
    fn port_zero_fails() {
        let err = validate_address("192.168.1.1:0");
        assert!(err.is_err());
        assert!(err.unwrap_err().is_validation());
    }

    #[test]
    fn port_too_large_fails() {
        assert!(validate_address("192.168.1.1:99999").is_err());
    }

    #[test]
    fn non_numeric_port_fails() {
        assert!(validate_address("192.168.1.1:adb").is_err());
    }

    // -- validate_pairing_code -----------------------------------------------

    #[test]
    fn valid_pairing_code_passes() {
        assert!(validate_pairing_code("123456").is_ok());
        assert!(validate_pairing_code("000000").is_ok());
        assert!(validate_pairing_code("999999").is_ok());
    }

    #[test]
    fn pairing_code_too_short_fails() {
        let err = validate_pairing_code("12345");
        assert!(err.is_err());
        assert!(err.unwrap_err().is_validation());
    }

    #[test]
    fn pairing_code_too_long_fails() {
        assert!(validate_pairing_code("1234567").is_err());
    }

    #[test]
    fn pairing_code_with_letters_fails() {
        assert!(validate_pairing_code("12345a").is_err());
    }

    #[test]
    fn pairing_code_empty_fails() {
        assert!(validate_pairing_code("").is_err());
    }

    // -- parse_wifi_devices --------------------------------------------------

    #[test]
    fn parses_single_wifi_device() {
        let output = "List of devices attached\n\
                      192.168.1.42:5555\tdevice product:mustang model:Pixel_10_XL device:mustang transport_id:3\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].address, "192.168.1.42:5555");
        assert_eq!(devices[0].serial, "192.168.1.42:5555");
        assert_eq!(devices[0].model.as_deref(), Some("Pixel 10 XL"));
        assert!(devices[0].connected);
    }

    #[test]
    fn excludes_usb_devices() {
        let output = "List of devices attached\n\
                      19281FDEE000GS\tdevice product:mustang model:Pixel_10_XL device:mustang transport_id:1\n\
                      192.168.1.42:5555\tdevice model:Pixel_8 transport_id:2\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 1, "only the WiFi device should be included");
        assert_eq!(devices[0].address, "192.168.1.42:5555");
    }

    #[test]
    fn excludes_emulator_devices() {
        let output = "List of devices attached\n\
                      emulator-5554\tdevice product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 transport_id:1\n\
                      192.168.1.10:5555\tdevice model:Pixel_7 transport_id:2\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].address, "192.168.1.10:5555");
    }

    #[test]
    fn parses_unauthorized_wifi_device() {
        let output = "List of devices attached\n\
                      192.168.1.42:5555\tunauthorized transport_id:4\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 1);
        assert!(
            !devices[0].connected,
            "unauthorized device is not 'connected'"
        );
        assert!(devices[0].model.is_none());
    }

    #[test]
    fn parses_multiple_wifi_devices() {
        let output = "List of devices attached\n\
                      10.0.0.1:5555\tdevice model:Pixel_8 transport_id:2\n\
                      10.0.0.2:5555\tdevice model:Pixel_9_Pro transport_id:3\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 2);
        let addrs: Vec<&str> = devices.iter().map(|d| d.address.as_str()).collect();
        assert!(addrs.contains(&"10.0.0.1:5555"));
        assert!(addrs.contains(&"10.0.0.2:5555"));
    }

    #[test]
    fn empty_output_returns_empty_vec() {
        let output = "List of devices attached\n";
        let devices = parse_wifi_devices(output);
        assert!(devices.is_empty());
    }

    #[test]
    fn wifi_device_model_underscores_replaced() {
        let output = "List of devices attached\n\
                      192.168.1.5:5555\tdevice model:Galaxy_S25_Ultra transport_id:1\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices[0].model.as_deref(), Some("Galaxy S25 Ultra"));
    }

    #[test]
    fn wifi_device_without_model_has_none() {
        let output = "List of devices attached\n\
                      172.16.0.3:44321\tdevice transport_id:5\n";
        let devices = parse_wifi_devices(output);
        assert_eq!(devices.len(), 1);
        assert!(devices[0].model.is_none());
    }

    // -- check_adb_output_for_error ------------------------------------------

    #[test]
    fn clean_output_passes() {
        assert!(
            check_adb_output_for_error("connected to 192.168.1.1:5555", "192.168.1.1:5555").is_ok()
        );
    }

    #[test]
    fn error_prefix_in_output_fails() {
        let err = check_adb_output_for_error("error: cannot connect to daemon", "192.168.1.1:5555");
        assert!(err.is_err());
        assert!(matches!(
            err.unwrap_err(),
            WifiAdbError::ConnectionFailed { .. }
        ));
    }

    #[test]
    fn failed_to_prefix_fails() {
        let err =
            check_adb_output_for_error("failed to connect to 192.168.1.1:5555", "192.168.1.1:5555");
        assert!(err.is_err());
    }

    #[test]
    fn error_case_insensitive() {
        // "ERROR:" should also be caught
        let err = check_adb_output_for_error("ERROR: no route to host", "10.0.0.1:5555");
        assert!(err.is_err());
    }

    // -- WifiDevice serialisation --------------------------------------------

    #[test]
    fn wifi_device_serialises_camel_case() {
        let d = WifiDevice {
            address: "192.168.1.42:5555".into(),
            serial: "192.168.1.42:5555".into(),
            model: Some("Pixel 8".into()),
            connected: true,
        };
        let v = serde_json::to_value(&d).expect("serialise WifiDevice");
        assert!(v.get("address").is_some());
        assert!(v.get("serial").is_some());
        assert!(v.get("model").is_some());
        assert!(v.get("connected").is_some());
        // no snake_case keys for multi-word fields — only connected/address/serial/model here,
        // all single-word so the rename doesn't apply, but verify round-trip is clean:
        let back: WifiDevice = serde_json::from_value(v).expect("deserialise WifiDevice");
        assert_eq!(back.address, "192.168.1.42:5555");
        assert!(back.connected);
    }

    // -- WifiAdbError helpers ------------------------------------------------

    #[test]
    fn wifi_adb_error_invalid_address_is_validation() {
        let e = WifiAdbError::InvalidAddress {
            address: "bad".into(),
        };
        assert!(e.is_validation());
        assert!(!e.is_tool_not_found());
    }

    #[test]
    fn wifi_adb_error_invalid_code_is_validation() {
        let e = WifiAdbError::InvalidPairingCode { code: "abc".into() };
        assert!(e.is_validation());
    }

    #[test]
    fn wifi_adb_error_display_contains_address() {
        let e = WifiAdbError::InvalidAddress {
            address: "notanip".into(),
        };
        assert!(e.to_string().contains("notanip"));
    }
}
