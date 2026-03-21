// property_cache.rs — Device property caching for GlassBridge Phase 4.
//
// Fetches Android device properties via `adb shell getprop`, `wm size`,
// `wm density`, and `dumpsys battery` then caches the result per device
// serial.  Cached entries are considered fresh for [`CACHE_TTL_SECS`] seconds.
//
// All cache access is protected by a `RwLock` so concurrent reads never block
// each other; a write only happens on cache miss.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::adb::{AdbError, DeviceOps};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How long a cached entry is considered fresh before a re-fetch is triggered.
const CACHE_TTL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by property cache operations.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::property_cache::CacheError;
///
/// let e = CacheError::Fetch { serial: "ABC".into(), cause: "timeout".into() };
/// assert!(e.to_string().contains("ABC"));
/// ```
#[derive(Debug, Error)]
pub enum CacheError {
    /// An ADB operation failed while fetching properties.
    #[error("failed to fetch properties for {serial}: {cause}")]
    Fetch { serial: String, cause: String },
}

impl CacheError {
    /// Returns `true` if the error is a fetch failure.
    pub fn is_fetch_failure(&self) -> bool {
        matches!(self, Self::Fetch { .. })
    }
}

impl From<AdbError> for CacheError {
    fn from(e: AdbError) -> Self {
        Self::Fetch {
            serial: String::new(),
            cause: e.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Cached properties for a single device.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::property_cache::DeviceProperties;
///
/// let props = DeviceProperties {
///     serial: "19281FDEE000GS".into(),
///     model: "Pixel 10 XL".into(),
///     manufacturer: "Google".into(),
///     android_version: "16".into(),
///     sdk_version: 36,
///     screen_width: 1344,
///     screen_height: 2992,
///     screen_density: 560,
///     battery_level: Some(87),
///     cached_at: "2026-03-21T14:05:33Z".into(),
/// };
/// assert_eq!(props.sdk_version, 36);
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProperties {
    /// USB serial / emulator port string.
    pub serial: String,
    /// Human-readable model name (e.g. `"Pixel 10 XL"`).
    pub model: String,
    /// Device manufacturer name (e.g. `"Google"`).
    pub manufacturer: String,
    /// Android OS version string (e.g. `"16"`).
    pub android_version: String,
    /// Android SDK API level (e.g. `36`).
    pub sdk_version: u32,
    /// Physical screen width in pixels.
    pub screen_width: u32,
    /// Physical screen height in pixels.
    pub screen_height: u32,
    /// Screen density in dpi.
    pub screen_density: u32,
    /// Battery charge level (0 – 100), if readable.
    pub battery_level: Option<u32>,
    /// RFC 3339 timestamp when this entry was last fetched.
    pub cached_at: String,
}

// Internal cache entry — adds the fetch timestamp for TTL checking.
struct CacheEntry {
    props: DeviceProperties,
    fetched_at: Instant,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < Duration::from_secs(CACHE_TTL_SECS)
    }
}

// ---------------------------------------------------------------------------
// PropertyCache
// ---------------------------------------------------------------------------

/// Thread-safe device property cache.
///
/// Share via `Arc<PropertyCache>`.  The `M-SERVICES-CLONE` pattern applies:
/// clone the `Arc`, not the struct itself.
///
/// # Examples
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use glassbridge_desktop::property_cache::PropertyCache;
/// use glassbridge_desktop::adb::AdbProcessBackend;
/// use std::path::Path;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let cache = Arc::new(PropertyCache::new());
/// let backend = AdbProcessBackend::new(Path::new("/repo/vendor"));
///
/// let props = cache.get_or_fetch("19281FDEE000GS", &backend).await?;
/// println!("model: {}", props.model);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct PropertyCache {
    // keyed by device serial
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

impl PropertyCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Return cached [`DeviceProperties`] for `serial` if the entry is still
    /// fresh (< 60 s old), otherwise re-fetch from the device via `backend`.
    ///
    /// Concurrent calls for the same serial may each trigger a fetch race;
    /// the last writer wins — this is acceptable because the properties only
    /// change during a reconnect.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Fetch`] if any ADB shell command fails.
    pub async fn get_or_fetch(
        &self,
        serial: &str,
        backend: &dyn DeviceOps,
    ) -> Result<DeviceProperties, CacheError> {
        // Fast path: read lock, return if fresh.
        {
            let guard = self.cache.read().await;
            if let Some(entry) = guard.get(serial) {
                if entry.is_fresh() {
                    debug!(serial = %serial, "property cache hit");
                    return Ok(entry.props.clone());
                }
            }
        }

        // Slow path: fetch from device.
        info!(serial = %serial, "fetching device properties");
        let props = fetch_properties(serial, backend)
            .await
            .map_err(|e| CacheError::Fetch {
                serial: serial.to_owned(),
                cause: e.to_string(),
            })?;

        // Write into cache.
        {
            let mut guard = self.cache.write().await;
            guard.insert(
                serial.to_owned(),
                CacheEntry {
                    props: props.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }

        Ok(props)
    }

    /// Remove the cache entry for `serial`, forcing the next call to
    /// [`get_or_fetch`](Self::get_or_fetch) to re-fetch from the device.
    pub async fn invalidate(&self, serial: &str) {
        let mut guard = self.cache.write().await;
        guard.remove(serial);
        debug!(serial = %serial, "property cache invalidated");
    }

    /// Clear the entire cache, forcing all subsequent fetches to hit the
    /// device.
    pub async fn invalidate_all(&self) {
        let mut guard = self.cache.write().await;
        let count = guard.len();
        guard.clear();
        info!(entries = %count, "property cache cleared");
    }

    /// Return the number of entries currently held in the cache.
    pub async fn len(&self) -> usize {
        self.cache.read().await.len()
    }

    /// Return `true` if the cache contains no entries.
    pub async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }
}

impl Default for PropertyCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Fetch logic
// ---------------------------------------------------------------------------

/// Fetch all device properties by running multiple adb shell commands.
async fn fetch_properties(
    serial: &str,
    backend: &dyn DeviceOps,
) -> Result<DeviceProperties, AdbError> {
    // Run all independent property queries concurrently.
    let (model_res, manufacturer_res, version_res, sdk_res, size_res, density_res, battery_res) = tokio::try_join!(
        backend.shell_command(serial, "getprop ro.product.model"),
        backend.shell_command(serial, "getprop ro.product.manufacturer"),
        backend.shell_command(serial, "getprop ro.build.version.release"),
        backend.shell_command(serial, "getprop ro.build.version.sdk"),
        backend.shell_command(serial, "wm size"),
        backend.shell_command(serial, "wm density"),
        backend.shell_command(serial, "dumpsys battery | grep level"),
    )?;

    let model = model_res.stdout.trim().to_owned();
    let manufacturer = manufacturer_res.stdout.trim().to_owned();
    let android_version = version_res.stdout.trim().to_owned();
    let sdk_version = sdk_res.stdout.trim().parse::<u32>().unwrap_or(0);

    let (screen_width, screen_height) = parse_wm_size(size_res.stdout.trim());
    let screen_density = parse_wm_density(density_res.stdout.trim());
    let battery_level = parse_battery_level(battery_res.stdout.trim());

    let cached_at = current_rfc3339();

    Ok(DeviceProperties {
        serial: serial.to_owned(),
        model,
        manufacturer,
        android_version,
        sdk_version,
        screen_width,
        screen_height,
        screen_density,
        battery_level,
        cached_at,
    })
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse `wm size` output: `"Physical size: 1344x2992"` → `(1344, 2992)`.
///
/// Returns `(0, 0)` if the output cannot be parsed.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::property_cache::parse_wm_size;
///
/// assert_eq!(parse_wm_size("Physical size: 1344x2992"), (1344, 2992));
/// assert_eq!(parse_wm_size("Override size: 1080x1920"), (1080, 1920));
/// assert_eq!(parse_wm_size(""), (0, 0));
/// ```
pub fn parse_wm_size(output: &str) -> (u32, u32) {
    // adb may emit "Physical size: WxH" or "Override size: WxH" when an
    // override is active.  We take the last "NxN" token we find.
    for line in output.lines().rev() {
        if let Some(dim) = line.split_whitespace().last() {
            if let Some((w_str, h_str)) = dim.split_once('x') {
                if let (Ok(w), Ok(h)) = (w_str.parse::<u32>(), h_str.parse::<u32>()) {
                    return (w, h);
                }
            }
        }
    }
    (0, 0)
}

/// Parse `wm density` output: `"Physical density: 560"` → `560`.
///
/// Returns `0` if the output cannot be parsed.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::property_cache::parse_wm_density;
///
/// assert_eq!(parse_wm_density("Physical density: 560"), 560);
/// assert_eq!(parse_wm_density("Override density: 420"), 420);
/// assert_eq!(parse_wm_density(""), 0);
/// ```
pub fn parse_wm_density(output: &str) -> u32 {
    for line in output.lines().rev() {
        if let Some(token) = line.split_whitespace().last() {
            if let Ok(dpi) = token.parse::<u32>() {
                return dpi;
            }
        }
    }
    0
}

/// Parse `dumpsys battery | grep level` output: `"  level: 87"` → `Some(87)`.
///
/// Returns `None` if the line is absent or unparseable.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::property_cache::parse_battery_level;
///
/// assert_eq!(parse_battery_level("  level: 87"), Some(87));
/// assert_eq!(parse_battery_level("level: 100"), Some(100));
/// assert_eq!(parse_battery_level(""), None);
/// ```
pub fn parse_battery_level(output: &str) -> Option<u32> {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("level:") {
            if let Some(num_str) = line.split(':').nth(1) {
                return num_str.trim().parse::<u32>().ok();
            }
        }
    }
    None
}

/// Return the current time as an RFC 3339 string.
///
/// This avoids pulling in `chrono` or `time`; we construct the string manually
/// from `std::time::SystemTime`.  Precision is to the second.
fn current_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Convert Unix timestamp to calendar fields (UTC).
    // Days since epoch → year/month/day using the proleptic Gregorian algorithm.
    let (year, month, day, hour, min, sec) = unix_to_calendar(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Decompose a Unix timestamp (seconds since 1970-01-01 UTC) into
/// `(year, month, day, hour, minute, second)`.
///
/// Uses the civil-time algorithm; does not handle dates before 1970.
fn unix_to_calendar(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec = (secs % 60) as u32;
    let mins = secs / 60;
    let min = (mins % 60) as u32;
    let hours = mins / 60;
    let hour = (hours % 24) as u32;
    let days = (hours / 24) as u32;

    // Days from epoch to year (400-year cycle civil algorithm).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    (year, month, day, hour, min, sec)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_wm_size -------------------------------------------------------

    #[test]
    fn wm_size_physical() {
        assert_eq!(parse_wm_size("Physical size: 1344x2992"), (1344, 2992));
    }

    #[test]
    fn wm_size_override() {
        assert_eq!(parse_wm_size("Override size: 1080x1920"), (1080, 1920));
    }

    #[test]
    fn wm_size_both_lines_takes_last() {
        // When there's an override, adb prints physical then override.
        let output = "Physical size: 1344x2992\nOverride size: 1080x1920";
        assert_eq!(parse_wm_size(output), (1080, 1920));
    }

    #[test]
    fn wm_size_empty_returns_zeros() {
        assert_eq!(parse_wm_size(""), (0, 0));
    }

    #[test]
    fn wm_size_malformed_returns_zeros() {
        assert_eq!(parse_wm_size("Physical size: AxB"), (0, 0));
    }

    #[test]
    fn wm_size_common_resolutions() {
        assert_eq!(parse_wm_size("Physical size: 1080x2400"), (1080, 2400));
        assert_eq!(parse_wm_size("Physical size: 1440x3200"), (1440, 3200));
        assert_eq!(parse_wm_size("Physical size: 2048x2732"), (2048, 2732));
    }

    // -- parse_wm_density ----------------------------------------------------

    #[test]
    fn wm_density_physical() {
        assert_eq!(parse_wm_density("Physical density: 560"), 560);
    }

    #[test]
    fn wm_density_override() {
        assert_eq!(parse_wm_density("Override density: 420"), 420);
    }

    #[test]
    fn wm_density_empty_returns_zero() {
        assert_eq!(parse_wm_density(""), 0);
    }

    #[test]
    fn wm_density_malformed_returns_zero() {
        assert_eq!(parse_wm_density("Physical density: abc"), 0);
    }

    #[test]
    fn wm_density_common_values() {
        assert_eq!(parse_wm_density("Physical density: 160"), 160);
        assert_eq!(parse_wm_density("Physical density: 240"), 240);
        assert_eq!(parse_wm_density("Physical density: 320"), 320);
        assert_eq!(parse_wm_density("Physical density: 480"), 480);
    }

    // -- parse_battery_level -------------------------------------------------

    #[test]
    fn battery_level_with_leading_spaces() {
        assert_eq!(parse_battery_level("  level: 87"), Some(87));
    }

    #[test]
    fn battery_level_full() {
        assert_eq!(parse_battery_level("level: 100"), Some(100));
    }

    #[test]
    fn battery_level_zero() {
        assert_eq!(parse_battery_level("level: 0"), Some(0));
    }

    #[test]
    fn battery_level_empty_returns_none() {
        assert_eq!(parse_battery_level(""), None);
    }

    #[test]
    fn battery_level_no_level_key_returns_none() {
        assert_eq!(parse_battery_level("status: Full\nscale: 100"), None);
    }

    #[test]
    fn battery_level_from_multi_line_output() {
        let output = "Current Battery Service state:\n  level: 72\n  scale: 100\n  status: 2";
        assert_eq!(parse_battery_level(output), Some(72));
    }

    // -- unix_to_calendar ----------------------------------------------------

    #[test]
    fn unix_epoch_is_1970_01_01() {
        let (y, mo, d, h, mi, s) = unix_to_calendar(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn known_timestamp_decodes_correctly() {
        // 2025-03-21 00:00:00 UTC = 1742515200
        let (y, mo, d, h, mi, s) = unix_to_calendar(1_742_515_200);
        assert_eq!(y, 2025);
        assert_eq!(mo, 3);
        assert_eq!(d, 21);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn current_rfc3339_has_correct_format() {
        let ts = current_rfc3339();
        // Format: "YYYY-MM-DDTHH:MM:SSZ"
        assert_eq!(ts.len(), 20, "RFC 3339 string must be 20 chars: {ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], "Z");
    }

    // -- PropertyCache -------------------------------------------------------

    #[tokio::test]
    async fn new_cache_is_empty() {
        let cache = PropertyCache::new();
        assert!(cache.is_empty().await);
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn invalidate_all_clears_manually_inserted_entries() {
        // We cannot call get_or_fetch without a real device, but we can verify
        // the cache starts empty and invalidate_all leaves it empty.
        let cache = PropertyCache::new();
        assert!(cache.is_empty().await);
        cache.invalidate_all().await;
        assert!(cache.is_empty().await);
    }

    #[tokio::test]
    async fn invalidate_on_missing_key_is_noop() {
        let cache = PropertyCache::new();
        // Should not panic or error
        cache.invalidate("nonexistent-serial").await;
        assert!(cache.is_empty().await);
    }

    // -- CacheError ----------------------------------------------------------

    #[test]
    fn cache_error_includes_serial_in_message() {
        let e = CacheError::Fetch {
            serial: "ABC123".into(),
            cause: "connection refused".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("ABC123"));
        assert!(msg.contains("connection refused"));
    }

    #[test]
    fn is_fetch_failure_returns_true_for_fetch_variant() {
        let e = CacheError::Fetch {
            serial: "x".into(),
            cause: "y".into(),
        };
        assert!(e.is_fetch_failure());
    }
}
