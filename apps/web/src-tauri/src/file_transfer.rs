// file_transfer.rs — ADB file push/pull for GlassBridge Phase 4.
//
// Wraps `adb push` and `adb pull` via `tokio::process::Command`.  Optional
// progress reporting is driven by parsing the speed/percentage tokens that
// appear in adb's stderr stream.
//
// Native ADB SYNC protocol support (direct TCP without shelling out) is
// deferred to Phase 2+ per the architecture plan.

use std::path::{Path, PathBuf};
use std::time::Instant;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from file push/pull operations.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::file_transfer::TransferError;
///
/// let e = TransferError::LocalPath("not found".into());
/// assert!(e.to_string().contains("not found"));
/// ```
#[derive(Debug, Error)]
pub enum TransferError {
    /// The local path is missing or inaccessible.
    #[error("local path error: {0}")]
    LocalPath(String),

    /// The adb process could not be spawned.
    #[error("failed to spawn adb: {0}")]
    Spawn(String),

    /// adb exited with a non-zero code.
    #[error("adb exited with code {exit_code}: {stderr}")]
    AdbFailure { exit_code: i32, stderr: String },

    /// An I/O error occurred while reading adb's output.
    #[error("I/O error reading adb output: {0}")]
    Io(#[from] std::io::Error),
}

impl TransferError {
    /// Returns `true` if the error is an adb non-zero exit.
    pub fn is_adb_failure(&self) -> bool {
        matches!(self, Self::AdbFailure { .. })
    }
}

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Real-time transfer progress snapshot.
///
/// Populated from adb's stderr output where available; fields are `None` when
/// adb has not yet emitted sufficient data.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::file_transfer::TransferProgress;
///
/// let p = TransferProgress {
///     bytes_transferred: 1_048_576,
///     total_bytes: Some(4_194_304),
///     percent: Some(25.0),
///     speed_bps: 524_288,
/// };
/// assert_eq!(p.percent, Some(25.0));
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total file size in bytes (if known).
    pub total_bytes: Option<u64>,
    /// Transfer completion percentage (0.0 – 100.0, if known).
    pub percent: Option<f32>,
    /// Instantaneous throughput in bytes per second.
    pub speed_bps: u64,
}

impl TransferProgress {
    pub(crate) fn zero() -> Self {
        Self {
            bytes_transferred: 0,
            total_bytes: None,
            percent: None,
            speed_bps: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// FileTransfer
// ---------------------------------------------------------------------------

/// Stateless helper for ADB file push/pull operations.
///
/// All methods are `async` and take a `progress_tx` watch channel so callers
/// can receive live progress without polling.  Pass `None` to skip progress
/// reporting.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::file_transfer::FileTransfer;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// FileTransfer::push(
///     "19281FDEE000GS",
///     Path::new("C:/Users/david/photo.jpg"),
///     "/sdcard/DCIM/photo.jpg",
///     Path::new("/repo/vendor"),
///     None,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub struct FileTransfer;

impl FileTransfer {
    /// Push a local file to the device.
    ///
    /// Runs `adb -s <serial> push <local_path> <remote_path>`.
    ///
    /// # Errors
    ///
    /// - [`TransferError::LocalPath`] if `local_path` does not exist.
    /// - [`TransferError::Spawn`] if `adb` cannot be started.
    /// - [`TransferError::AdbFailure`] if `adb push` exits with a non-zero code.
    pub async fn push(
        serial: &str,
        local_path: &Path,
        remote_path: &str,
        vendor_root: &Path,
        progress_tx: Option<watch::Sender<TransferProgress>>,
    ) -> Result<(), TransferError> {
        if !local_path.exists() {
            return Err(TransferError::LocalPath(format!(
                "path does not exist: {}",
                local_path.display()
            )));
        }

        let total_bytes = local_path.metadata().ok().map(|m| m.len());

        info!(
            serial = %serial,
            local = %local_path.display(),
            remote = %remote_path,
            "starting adb push"
        );

        let adb = adb_binary_path(vendor_root);
        let local_str = local_path.to_string_lossy();

        run_transfer(
            &adb,
            serial,
            &[local_str.as_ref(), remote_path],
            "push",
            total_bytes,
            progress_tx,
        )
        .await
    }

    /// Pull a remote file from the device to the local filesystem.
    ///
    /// Runs `adb -s <serial> pull <remote_path> <local_path>`.
    ///
    /// The destination directory must already exist; adb itself is responsible
    /// for creating the local file.
    ///
    /// # Errors
    ///
    /// - [`TransferError::Spawn`] if `adb` cannot be started.
    /// - [`TransferError::AdbFailure`] if `adb pull` exits with a non-zero code.
    pub async fn pull(
        serial: &str,
        remote_path: &str,
        local_path: &Path,
        vendor_root: &Path,
        progress_tx: Option<watch::Sender<TransferProgress>>,
    ) -> Result<(), TransferError> {
        info!(
            serial = %serial,
            remote = %remote_path,
            local = %local_path.display(),
            "starting adb pull"
        );

        let adb = adb_binary_path(vendor_root);
        let local_str = local_path.to_string_lossy();

        run_transfer(
            &adb,
            serial,
            &[remote_path, local_str.as_ref()],
            "pull",
            None,
            progress_tx,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Run `adb -s <serial> <verb> <extra_args…>`, streaming stderr to update
/// progress, and wait for exit.
async fn run_transfer(
    adb: &Path,
    serial: &str,
    extra_args: &[&str],
    verb: &str,
    total_bytes: Option<u64>,
    progress_tx: Option<watch::Sender<TransferProgress>>,
) -> Result<(), TransferError> {
    let mut args = vec!["-s", serial, verb];
    args.extend_from_slice(extra_args);

    let mut child = Command::new(adb)
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| TransferError::Spawn(e.to_string()))?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| TransferError::Spawn("stderr pipe unavailable".into()))?;

    // Collect stderr text for error messages while optionally parsing progress.
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut stderr_collected = String::new();
    let started_at = Instant::now();

    while let Ok(Some(line)) = stderr_lines.next_line().await {
        debug!(verb = %verb, "adb stderr: {line}");
        stderr_collected.push_str(&line);
        stderr_collected.push('\n');

        if let Some(ref tx) = progress_tx {
            if let Some(progress) = parse_progress_line(&line, total_bytes, started_at) {
                let _ = tx.send(progress);
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| TransferError::Spawn(format!("failed to wait for adb: {e}")))?;

    let exit_code = status.code().unwrap_or(-1);

    if exit_code != 0 {
        return Err(TransferError::AdbFailure {
            exit_code,
            stderr: stderr_collected.trim().to_owned(),
        });
    }

    // Emit a final 100 % progress snapshot.
    if let Some(tx) = progress_tx {
        let elapsed = started_at.elapsed();
        let bytes = total_bytes.unwrap_or(0);
        let speed = if elapsed.as_secs_f64() > 0.0 {
            (bytes as f64 / elapsed.as_secs_f64()) as u64
        } else {
            0
        };
        let _ = tx.send(TransferProgress {
            bytes_transferred: bytes,
            total_bytes,
            percent: total_bytes.map(|_| 100.0),
            speed_bps: speed,
        });
    }

    info!(verb = %verb, "adb transfer complete");
    Ok(())
}

/// Parse a progress line emitted by adb to stderr.
///
/// adb emits lines in various formats depending on version and platform.
/// Common patterns seen in practice:
///
/// ```text
/// [100%] /sdcard/DCIM/photo.jpg
/// photo.jpg: 1 file pushed, 0 skipped. 12.5 MB/s (1048576 bytes in 0.080s)
/// ```
///
/// We attempt to extract byte count and percentage from the `[N%]` prefix and
/// the `(N bytes in Xs)` suffix.  Everything else is a best-effort parse.
fn parse_progress_line(
    line: &str,
    total_bytes: Option<u64>,
    started_at: Instant,
) -> Option<TransferProgress> {
    let elapsed = started_at.elapsed();

    // Pattern 1: "[N%] /path/to/file"
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(pct_end) = rest.find('%') {
            if let Ok(pct) = rest[..pct_end].trim().parse::<f32>() {
                let bytes_transferred = total_bytes
                    .map(|total| (total as f64 * pct as f64 / 100.0) as u64)
                    .unwrap_or(0);
                let speed = if elapsed.as_secs_f64() > 0.0 {
                    (bytes_transferred as f64 / elapsed.as_secs_f64()) as u64
                } else {
                    0
                };
                return Some(TransferProgress {
                    bytes_transferred,
                    total_bytes,
                    percent: Some(pct),
                    speed_bps: speed,
                });
            }
        }
    }

    // Pattern 2: "N MB/s (BYTES bytes in Xs)"  — final summary line
    // Example:   "12.5 MB/s (1048576 bytes in 0.080s)"
    if let (Some(bytes_start), Some(bytes_end)) = (line.find('('), line.find(" bytes")) {
        let bytes_str = &line[bytes_start + 1..bytes_end];
        if let Ok(bytes) = bytes_str.trim().parse::<u64>() {
            let speed = if elapsed.as_secs_f64() > 0.0 {
                (bytes as f64 / elapsed.as_secs_f64()) as u64
            } else {
                0
            };
            return Some(TransferProgress {
                bytes_transferred: bytes,
                total_bytes: Some(bytes),
                percent: Some(100.0),
                speed_bps: speed,
            });
        }
    }

    None
}

/// Resolve the vendored `adb` binary path.
fn adb_binary_path(vendor_root: &Path) -> PathBuf {
    let candidate = if cfg!(target_os = "windows") {
        vendor_root.join("adb.exe")
    } else {
        vendor_root.join("adb")
    };
    if candidate.exists() {
        candidate
    } else {
        PathBuf::from(if cfg!(target_os = "windows") {
            "adb.exe"
        } else {
            "adb"
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mock_started() -> Instant {
        // Simulate 0.5 s elapsed for speed calculation in tests.
        // Instant doesn't allow direct construction, so we subtract from now.
        Instant::now() - Duration::from_millis(500)
    }

    // -- parse_progress_line -------------------------------------------------

    #[test]
    fn parses_percentage_prefix_line() {
        let started = mock_started();
        let line = "[  50%] /sdcard/DCIM/photo.jpg";
        let progress = parse_progress_line(line, Some(2_000_000), started)
            .expect("should parse percentage line");
        assert_eq!(progress.percent, Some(50.0));
        assert_eq!(progress.total_bytes, Some(2_000_000));
        assert_eq!(progress.bytes_transferred, 1_000_000);
    }

    #[test]
    fn parses_100_percent_line() {
        let started = mock_started();
        let line = "[100%] /sdcard/DCIM/photo.jpg";
        let progress =
            parse_progress_line(line, Some(4_096), started).expect("should parse 100% line");
        assert_eq!(progress.percent, Some(100.0));
    }

    #[test]
    fn parses_bytes_summary_line() {
        let started = mock_started();
        let line = "photo.jpg: 1 file pushed. 12.5 MB/s (1048576 bytes in 0.080s)";
        let progress =
            parse_progress_line(line, None, started).expect("should parse bytes summary line");
        assert_eq!(progress.bytes_transferred, 1_048_576);
        assert_eq!(progress.total_bytes, Some(1_048_576));
        assert_eq!(progress.percent, Some(100.0));
    }

    #[test]
    fn non_progress_line_returns_none() {
        let started = mock_started();
        let line = "adb: error: failed to copy 'local.txt' to '/sdcard/nope.txt'";
        let result = parse_progress_line(line, None, started);
        assert!(result.is_none());
    }

    #[test]
    fn empty_line_returns_none() {
        let started = mock_started();
        assert!(parse_progress_line("", None, started).is_none());
    }

    // -- TransferProgress ----------------------------------------------------

    #[test]
    fn transfer_progress_zero_is_default_state() {
        let p = TransferProgress::zero();
        assert_eq!(p.bytes_transferred, 0);
        assert_eq!(p.total_bytes, None);
        assert_eq!(p.percent, None);
        assert_eq!(p.speed_bps, 0);
    }

    // -- TransferError -------------------------------------------------------

    #[test]
    fn is_adb_failure_only_for_adb_failure_variant() {
        let e = TransferError::AdbFailure {
            exit_code: 1,
            stderr: "error".into(),
        };
        assert!(e.is_adb_failure());

        let e2 = TransferError::LocalPath("missing".into());
        assert!(!e2.is_adb_failure());

        let e3 = TransferError::Spawn("spawn error".into());
        assert!(!e3.is_adb_failure());
    }

    #[test]
    fn adb_failure_message_includes_exit_code() {
        let e = TransferError::AdbFailure {
            exit_code: 127,
            stderr: "command not found".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("127"), "message should include exit code");
        assert!(msg.contains("command not found"));
    }

    // -- speed calculation via parse_progress_line ---------------------------

    #[test]
    fn speed_is_nonzero_after_elapsed_time() {
        let started = Instant::now() - Duration::from_secs(1);
        let line = "[100%] /sdcard/video.mp4";
        let progress = parse_progress_line(line, Some(10_000_000), started).expect("should parse");
        // After 1 s of 10 MB transfer, speed should be approximately 10 MB/s.
        assert!(
            progress.speed_bps > 0,
            "speed should be non-zero: got {}",
            progress.speed_bps
        );
    }

    #[test]
    fn percent_zero_gives_zero_bytes_transferred() {
        let started = mock_started();
        let line = "[  0%] /sdcard/bigfile.bin";
        let progress =
            parse_progress_line(line, Some(1_000_000), started).expect("should parse 0% line");
        assert_eq!(progress.percent, Some(0.0));
        assert_eq!(progress.bytes_transferred, 0);
    }
}
