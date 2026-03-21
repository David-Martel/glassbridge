// logcat.rs — streaming logcat implementation for GlassBridge Phase 4.
//
// Spawns `adb -s <serial> logcat -v threadtime` as a background tokio task,
// parses every emitted line into a [`LogcatEntry`], and re-broadcasts it on a
// [`tokio::sync::broadcast`] channel so multiple WebSocket subscribers can all
// receive the same stream without extra I/O.
//
// # Lifecycle
//
// ```text
// LogcatStream::start() → spawns tokio task + adb child
//     │
//     ├─ task reads stdout lines → parses → sender.send()
//     │
// LogcatStream::subscribe() → broadcast::Receiver<LogcatEntry>
//     │
// LogcatStream::stop()      → aborts task, kills child
// ```

use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by logcat streaming operations.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::logcat::LogcatError;
///
/// let e = LogcatError::Spawn("permission denied".into());
/// assert!(e.to_string().contains("permission denied"));
/// ```
#[derive(Debug, Error)]
pub enum LogcatError {
    /// The `adb logcat` process could not be spawned.
    #[error("failed to spawn adb logcat: {0}")]
    Spawn(String),

    /// The stream is already running; call `stop()` before `start()` again.
    #[error("logcat stream already running for serial {serial}")]
    AlreadyRunning { serial: String },
}

impl LogcatError {
    /// Returns `true` if the error is because a stream is already running.
    pub fn is_already_running(&self) -> bool {
        matches!(self, Self::AlreadyRunning { .. })
    }
}

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Log priority level, matching the single-character codes in logcat output.
///
/// The numeric ordering mirrors Android's `android.util.Log` constants so
/// `min_priority` comparisons work with `<=`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum LogPriority {
    /// `V` — verbose (lowest).
    Verbose = 2,
    /// `D` — debug.
    Debug = 3,
    /// `I` — informational.
    Info = 4,
    /// `W` — warning.
    Warn = 5,
    /// `E` — error.
    Error = 6,
    /// `F` — fatal.
    Fatal = 7,
    /// `S` — silent (suppress all output).
    Silent = 8,
}

impl LogPriority {
    /// Parse the single-character logcat priority code.
    ///
    /// Returns `None` for unrecognised characters.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use glassbridge_desktop::logcat::LogPriority;
    ///
    /// assert_eq!(LogPriority::from_char('E'), Some(LogPriority::Error));
    /// assert_eq!(LogPriority::from_char('?'), None);
    /// ```
    #[must_use]
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            'V' => Some(Self::Verbose),
            'D' => Some(Self::Debug),
            'I' => Some(Self::Info),
            'W' => Some(Self::Warn),
            'E' => Some(Self::Error),
            'F' => Some(Self::Fatal),
            'S' => Some(Self::Silent),
            _ => None,
        }
    }
}

/// A single parsed logcat line in `threadtime` format.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::logcat::{LogcatEntry, LogPriority};
///
/// let entry = LogcatEntry {
///     timestamp: "03-21 14:05:33.123".into(),
///     pid: 1234,
///     tid: 1235,
///     priority: LogPriority::Info,
///     tag: "ActivityManager".into(),
///     message: "Start proc com.example.app".into(),
/// };
/// assert_eq!(entry.priority, LogPriority::Info);
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogcatEntry {
    /// Timestamp string as emitted by logcat (`MM-DD HH:MM:SS.mmm`).
    pub timestamp: String,
    /// Process ID of the logging process.
    pub pid: u32,
    /// Thread ID of the logging thread.
    pub tid: u32,
    /// Log priority level.
    pub priority: LogPriority,
    /// Log tag (may contain spaces).
    pub tag: String,
    /// Log message body.
    pub message: String,
}

/// Filter criteria applied before an entry is broadcast.
///
/// All populated fields must match; an empty `LogcatFilter` passes everything.
#[derive(Debug, Default, Clone)]
pub struct LogcatFilter {
    /// Only emit entries whose `tag` is in this list (case-sensitive).
    /// An empty `Vec` means no tag filtering.
    pub tags: Vec<String>,
    /// Minimum priority level to emit.  Entries with a lower priority are
    /// dropped silently.
    pub min_priority: Option<LogPriority>,
    /// Only emit entries from this specific PID.
    pub pid: Option<u32>,
}

impl LogcatFilter {
    /// Returns `true` if `entry` passes all active filter criteria.
    #[must_use]
    fn matches(&self, entry: &LogcatEntry) -> bool {
        if let Some(min) = self.min_priority {
            if entry.priority < min {
                return false;
            }
        }
        if let Some(pid) = self.pid {
            if entry.pid != pid {
                return false;
            }
        }
        if !self.tags.is_empty() && !self.tags.contains(&entry.tag) {
            return false;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// LogcatStream
// ---------------------------------------------------------------------------

/// Manages a running `adb logcat` subprocess and re-broadcasts its output.
///
/// Obtain log entries by calling [`LogcatStream::subscribe`] before or after
/// starting the stream.  Receivers that fall behind will receive a
/// [`tokio::sync::broadcast::error::RecvError::Lagged`] error rather than
/// blocking the producer.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use glassbridge_desktop::logcat::{LogcatStream, LogcatFilter};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let filter = LogcatFilter::default();
/// let mut stream = LogcatStream::start(
///     "19281FDEE000GS",
///     Path::new("/repo/vendor"),
///     filter,
/// ).await?;
///
/// let mut rx = stream.subscribe();
/// // Read entries in another task…
/// tokio::spawn(async move {
///     while let Ok(entry) = rx.recv().await {
///         println!("{} {}: {}", entry.tag, entry.priority as u8, entry.message);
///     }
/// });
///
/// stream.stop().await;
/// # Ok(())
/// # }
/// ```
pub struct LogcatStream {
    serial: String,
    sender: broadcast::Sender<LogcatEntry>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// Broadcast channel capacity: number of entries buffered before slow
/// subscribers start receiving `Lagged` errors.
const CHANNEL_CAPACITY: usize = 4_096;

impl LogcatStream {
    /// Start streaming logcat from `serial`.
    ///
    /// Spawns `adb -s <serial> logcat -v threadtime` via the vendored `adb`
    /// binary located relative to `vendor_root`, then launches a background
    /// tokio task that reads stdout line-by-line, parses each line into a
    /// [`LogcatEntry`], applies `filter`, and broadcasts matching entries.
    ///
    /// # Errors
    ///
    /// Returns [`LogcatError::Spawn`] if the adb process cannot be started.
    pub async fn start(
        serial: impl Into<String>,
        vendor_root: &Path,
        filter: LogcatFilter,
    ) -> Result<Self, LogcatError> {
        let serial = serial.into();
        let adb_path = adb_binary_path(vendor_root);

        info!(serial = %serial, "starting logcat stream");

        let mut child = Command::new(&adb_path)
            .args(["-s", &serial, "logcat", "-v", "threadtime"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| LogcatError::Spawn(e.to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LogcatError::Spawn("stdout pipe unavailable".into()))?;

        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        let tx = sender.clone();
        let serial_clone = serial.clone();

        let handle = tokio::spawn(async move {
            // Keep the child alive for the duration of this task.
            let _child = child;

            let mut lines = BufReader::new(stdout).lines();
            let mut pending_entry: Option<LogcatEntry> = None;

            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        // Attempt to parse a fresh timestamped line.
                        if let Some(entry) = parse_logcat_line(&line) {
                            // Flush the previous complete entry first.
                            if let Some(prev) = pending_entry.take() {
                                if filter.matches(&prev) {
                                    let _ = tx.send(prev);
                                }
                            }
                            pending_entry = Some(entry);
                        } else if let Some(ref mut entry) = pending_entry {
                            // Continuation line — append to the current message.
                            entry.message.push('\n');
                            entry.message.push_str(line.trim());
                        } else {
                            // Header line or unparseable — skip.
                            debug!(serial = %serial_clone, "skipping unparseable logcat line: {line}");
                        }
                    }
                    Ok(None) => {
                        // adb exited — flush last entry then terminate.
                        if let Some(prev) = pending_entry.take() {
                            if filter.matches(&prev) {
                                let _ = tx.send(prev);
                            }
                        }
                        info!(serial = %serial_clone, "logcat stream ended (adb exited)");
                        break;
                    }
                    Err(e) => {
                        warn!(serial = %serial_clone, error = %e, "logcat I/O error");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            serial,
            sender,
            handle: Some(handle),
        })
    }

    /// Subscribe to the logcat broadcast channel.
    ///
    /// The returned [`broadcast::Receiver`] will receive every future
    /// [`LogcatEntry`] that passes the filter configured at start time.
    /// Calling `subscribe()` before `start()` is not possible; call it on the
    /// returned `LogcatStream`.
    ///
    /// Slow receivers will see [`tokio::sync::broadcast::error::RecvError::Lagged`]
    /// once more than `CHANNEL_CAPACITY` entries accumulate unread.
    pub fn subscribe(&self) -> broadcast::Receiver<LogcatEntry> {
        self.sender.subscribe()
    }

    /// Returns the device serial this stream is attached to.
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Stop the stream, aborting the background task.
    ///
    /// Idempotent: calling `stop()` on an already-stopped stream is a no-op.
    pub async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            info!(serial = %self.serial, "stopping logcat stream");
            handle.abort();
            // Await to clean up the task — the AbortError is expected.
            let _ = handle.await;
        }
    }

    /// Returns `true` if the background task is still running.
    pub fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }
}

impl Drop for LogcatStream {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Resolve the `adb` binary path from `vendor_root`, using the platform
/// extension.  Falls back to `adb` on `$PATH` if the vendor binary is absent.
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

/// Parse one line of `logcat -v threadtime` output into a [`LogcatEntry`].
///
/// The threadtime format is:
/// ```text
/// MM-DD HH:MM:SS.mmm  PID  TID PRIORITY TAG              : message
/// ```
///
/// Returns `None` for header lines, blank lines, or lines that do not match
/// the expected format.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::logcat::{parse_logcat_line, LogPriority};
///
/// let line = "03-21 14:05:33.123  1234  1235 I ActivityManager: Start proc com.example";
/// let entry = parse_logcat_line(line).unwrap();
/// assert_eq!(entry.pid, 1234);
/// assert_eq!(entry.tid, 1235);
/// assert_eq!(entry.priority, LogPriority::Info);
/// assert_eq!(entry.tag, "ActivityManager");
/// assert_eq!(entry.message, "Start proc com.example");
/// ```
pub fn parse_logcat_line(line: &str) -> Option<LogcatEntry> {
    // Minimum meaningful length: "MM-DD HH:MM:SS.mmm P T P T: m" ≈ 30 chars
    if line.len() < 20 {
        return None;
    }

    // Consume the timestamp token: "MM-DD" + whitespace + "HH:MM:SS.mmm"
    let mut iter = line.splitn(2, ' ');
    let date_part = iter.next()?.trim();
    // Date part must look like MM-DD (5 chars, digit-dash-digit pattern)
    if date_part.len() != 5 || date_part.as_bytes().get(2) != Some(&b'-') {
        return None;
    }

    let rest = iter.next()?.trim_start();

    // Next token is the time: "HH:MM:SS.mmm"
    let mut rest_iter = rest.splitn(2, ' ');
    let time_part = rest_iter.next()?.trim();
    // Time part must contain at least one colon
    if !time_part.contains(':') {
        return None;
    }

    let timestamp = format!("{date_part} {time_part}");
    let rest = rest_iter.next()?.trim_start();

    // Now: "PID  TID PRIORITY TAG: message"
    // Split on whitespace, collecting tokens up to the colon-terminated tag.
    let mut tokens = rest.split_whitespace();

    let pid_str = tokens.next()?;
    let tid_str = tokens.next()?;
    let priority_str = tokens.next()?;

    let pid: u32 = pid_str.parse().ok()?;
    let tid: u32 = tid_str.parse().ok()?;

    let priority_char = priority_str.chars().next()?;
    let priority = LogPriority::from_char(priority_char)?;

    // Everything from the priority token's end to the first `: ` is the tag.
    // We need the position in the original `rest` string.
    // Strategy: advance past pid/tid/priority in `rest`, then find "TAG: message".
    //
    // Re-locate the start of the tag in `rest` by skipping exactly 3 whitespace-
    // separated tokens.
    let after_priority = {
        let mut remaining = rest;
        for _ in 0..3 {
            remaining = remaining.trim_start();
            let end = remaining
                .find(|c: char| c.is_whitespace())
                .unwrap_or(remaining.len());
            remaining = &remaining[end..];
        }
        remaining.trim_start()
    };

    // The rest is "TAG: message" — split on the first ": "
    let (tag_raw, message) = after_priority.split_once(": ")?;
    let tag = tag_raw.trim().to_owned();
    let message = message.to_owned();

    Some(LogcatEntry {
        timestamp,
        pid,
        tid,
        priority,
        tag,
        message,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_logcat_line ---------------------------------------------------

    #[test]
    fn parses_standard_info_line() {
        let line = "03-21 14:05:33.123  1234  1235 I ActivityManager: Start proc com.example.app";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.timestamp, "03-21 14:05:33.123");
        assert_eq!(entry.pid, 1234);
        assert_eq!(entry.tid, 1235);
        assert_eq!(entry.priority, LogPriority::Info);
        assert_eq!(entry.tag, "ActivityManager");
        assert_eq!(entry.message, "Start proc com.example.app");
    }

    #[test]
    fn parses_error_line() {
        let line = "01-01 00:00:01.000   100   101 E MyTag: something went wrong";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.priority, LogPriority::Error);
        assert_eq!(entry.tag, "MyTag");
        assert_eq!(entry.message, "something went wrong");
    }

    #[test]
    fn parses_warn_line() {
        let line = "12-31 23:59:59.999   555   556 W NetworkStack: retrying connection";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.priority, LogPriority::Warn);
        assert_eq!(entry.pid, 555);
        assert_eq!(entry.tid, 556);
    }

    #[test]
    fn parses_verbose_line() {
        let line = "06-15 08:12:00.001  9999  9998 V Verbose: trace output here";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.priority, LogPriority::Verbose);
    }

    #[test]
    fn parses_fatal_line() {
        let line = "02-28 11:11:11.111  7777  7778 F FATAL: SIGSEGV at 0xdeadbeef";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.priority, LogPriority::Fatal);
        assert_eq!(entry.tag, "FATAL");
    }

    #[test]
    fn message_with_colon_is_preserved() {
        let line = "03-21 14:05:33.123  1234  1235 I TagName: url: https://example.com";
        let entry = parse_logcat_line(line).expect("should parse");
        assert_eq!(entry.message, "url: https://example.com");
    }

    #[test]
    fn header_line_returns_none() {
        assert!(parse_logcat_line("--------- beginning of main").is_none());
    }

    #[test]
    fn blank_line_returns_none() {
        assert!(parse_logcat_line("").is_none());
    }

    #[test]
    fn short_line_returns_none() {
        assert!(parse_logcat_line("short").is_none());
    }

    #[test]
    fn unknown_priority_char_returns_none() {
        let line = "03-21 14:05:33.123  1234  1235 X TagName: some message";
        assert!(parse_logcat_line(line).is_none());
    }

    // -- LogPriority ---------------------------------------------------------

    #[test]
    fn log_priority_from_char_roundtrips() {
        let cases = [
            ('V', LogPriority::Verbose),
            ('D', LogPriority::Debug),
            ('I', LogPriority::Info),
            ('W', LogPriority::Warn),
            ('E', LogPriority::Error),
            ('F', LogPriority::Fatal),
            ('S', LogPriority::Silent),
        ];
        for (c, expected) in cases {
            assert_eq!(LogPriority::from_char(c), Some(expected));
        }
    }

    #[test]
    fn log_priority_unknown_returns_none() {
        for c in ['X', 'Z', '1', ' ', '\0'] {
            assert_eq!(LogPriority::from_char(c), None, "char {c:?} should be None");
        }
    }

    #[test]
    fn log_priority_ordering() {
        assert!(LogPriority::Verbose < LogPriority::Debug);
        assert!(LogPriority::Debug < LogPriority::Info);
        assert!(LogPriority::Info < LogPriority::Warn);
        assert!(LogPriority::Warn < LogPriority::Error);
        assert!(LogPriority::Error < LogPriority::Fatal);
        assert!(LogPriority::Fatal < LogPriority::Silent);
    }

    // -- LogcatFilter --------------------------------------------------------

    #[test]
    fn empty_filter_passes_everything() {
        let filter = LogcatFilter::default();
        let entry = make_entry(LogPriority::Verbose, "AnyTag", 1);
        assert!(filter.matches(&entry));
    }

    #[test]
    fn min_priority_filters_lower() {
        let filter = LogcatFilter {
            min_priority: Some(LogPriority::Warn),
            ..Default::default()
        };
        assert!(!filter.matches(&make_entry(LogPriority::Info, "T", 1)));
        assert!(!filter.matches(&make_entry(LogPriority::Verbose, "T", 1)));
        assert!(filter.matches(&make_entry(LogPriority::Warn, "T", 1)));
        assert!(filter.matches(&make_entry(LogPriority::Error, "T", 1)));
    }

    #[test]
    fn tag_filter_passes_matching_tag() {
        let filter = LogcatFilter {
            tags: vec!["ActivityManager".into(), "Zygote".into()],
            ..Default::default()
        };
        assert!(filter.matches(&make_entry(LogPriority::Info, "ActivityManager", 1)));
        assert!(filter.matches(&make_entry(LogPriority::Info, "Zygote", 1)));
        assert!(!filter.matches(&make_entry(LogPriority::Info, "OtherTag", 1)));
    }

    #[test]
    fn pid_filter_passes_matching_pid() {
        let filter = LogcatFilter {
            pid: Some(1234),
            ..Default::default()
        };
        assert!(filter.matches(&make_entry_pid(1234)));
        assert!(!filter.matches(&make_entry_pid(9999)));
    }

    #[test]
    fn combined_filter_requires_all_conditions() {
        let filter = LogcatFilter {
            tags: vec!["MyTag".into()],
            min_priority: Some(LogPriority::Warn),
            pid: Some(100),
        };
        // All conditions met
        let mut entry = make_entry(LogPriority::Error, "MyTag", 100);
        assert!(filter.matches(&entry));
        // Wrong tag
        entry.tag = "OtherTag".into();
        assert!(!filter.matches(&entry));
        // Restore tag, wrong priority
        entry.tag = "MyTag".into();
        entry.priority = LogPriority::Debug;
        assert!(!filter.matches(&entry));
        // Restore priority, wrong pid
        entry.priority = LogPriority::Error;
        entry.pid = 999;
        assert!(!filter.matches(&entry));
    }

    // -- helpers -------------------------------------------------------------

    fn make_entry(priority: LogPriority, tag: &str, pid: u32) -> LogcatEntry {
        LogcatEntry {
            timestamp: "01-01 00:00:00.000".into(),
            pid,
            tid: pid,
            priority,
            tag: tag.into(),
            message: "test".into(),
        }
    }

    fn make_entry_pid(pid: u32) -> LogcatEntry {
        make_entry(LogPriority::Info, "Tag", pid)
    }
}
