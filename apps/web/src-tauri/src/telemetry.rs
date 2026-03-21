// telemetry.rs — Performance telemetry for GlassBridge Phase 4.
//
// Collects timing and success/failure data for ADB operations.  The
// [`TelemetryCollector`] is designed to be shared via `Arc<TelemetryCollector>`
// (M-SERVICES-CLONE pattern) — the `entries` ring-buffer is protected by a
// `Mutex` while the counters use `AtomicU64` for lock-free increment.
//
// # Design notes
//
// - The ring-buffer is bounded by [`MAX_ENTRIES`] to cap memory usage.
// - `p95_latency_ms` is computed over the current ring-buffer snapshot, not a
//   rolling histogram, which is sufficient for interactive tooling.
// - The `record` path only acquires the `Mutex` long enough to push one entry
//   and truncate the buffer — no sorting or statistics are done under the lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tracing::debug;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Maximum number of [`OperationTelemetry`] entries retained in the
/// ring-buffer before the oldest entries are dropped.
const MAX_ENTRIES: usize = 10_000;

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// A single timed ADB (or device-related) operation.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::telemetry::OperationTelemetry;
///
/// let entry = OperationTelemetry {
///     operation: "list_devices".into(),
///     duration_ms: 42,
///     success: true,
///     backend: "process".into(),
/// };
/// assert!(entry.success);
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationTelemetry {
    /// Human-readable operation name, e.g. `"list_devices"` or `"push"`.
    pub operation: String,
    /// Wall-clock duration of the operation in milliseconds.
    pub duration_ms: u64,
    /// `true` if the operation succeeded; `false` if it returned an error.
    pub success: bool,
    /// Which backend handled the operation: `"process"` or `"native"`.
    pub backend: String,
}

/// Aggregated statistics computed from the current [`TelemetryCollector`]
/// ring-buffer snapshot.
///
/// # Examples
///
/// ```rust
/// use glassbridge_desktop::telemetry::TelemetrySummary;
/// use std::collections::HashMap;
///
/// let summary = TelemetrySummary {
///     total_operations: 100,
///     total_errors: 5,
///     avg_latency_ms: 38.4,
///     p95_latency_ms: 120.0,
///     operations_per_backend: HashMap::from([
///         ("process".into(), 100u64),
///     ]),
/// };
/// assert_eq!(summary.total_operations, 100);
/// ```
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySummary {
    /// Lifetime count of operations recorded (includes entries rolled off the
    /// ring-buffer).
    pub total_operations: u64,
    /// Lifetime count of failed operations.
    pub total_errors: u64,
    /// Mean duration in ms over the current ring-buffer snapshot.
    pub avg_latency_ms: f64,
    /// 95th-percentile duration in ms over the current ring-buffer snapshot.
    pub p95_latency_ms: f64,
    /// Operation count grouped by `backend` within the ring-buffer snapshot.
    pub operations_per_backend: HashMap<String, u64>,
}

// ---------------------------------------------------------------------------
// TelemetryCollector
// ---------------------------------------------------------------------------

/// Collects, aggregates, and exposes performance telemetry for ADB operations.
///
/// Share via `Arc<TelemetryCollector>` (the `M-SERVICES-CLONE` pattern).
/// All methods are `&self` — no external `mut` required.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use glassbridge_desktop::telemetry::{TelemetryCollector, OperationTelemetry};
///
/// let collector = Arc::new(TelemetryCollector::new());
///
/// collector.record(OperationTelemetry {
///     operation: "list_devices".into(),
///     duration_ms: 55,
///     success: true,
///     backend: "process".into(),
/// });
///
/// let summary = collector.summary();
/// assert_eq!(summary.total_operations, 1);
/// assert_eq!(summary.total_errors, 0);
/// ```
pub struct TelemetryCollector {
    entries: Mutex<Vec<OperationTelemetry>>,
    total_ops: AtomicU64,
    total_errors: AtomicU64,
}

impl TelemetryCollector {
    /// Create a new, empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::with_capacity(256)),
            total_ops: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
        }
    }

    /// Record a completed operation.
    ///
    /// This is the hot path — it increments two atomics, acquires the Mutex
    /// for a single `push`, and drops the lock.  If the ring-buffer exceeds
    /// [`MAX_ENTRIES`], the oldest half is removed to bound memory usage.
    pub fn record(&self, entry: OperationTelemetry) {
        debug!(
            op = %entry.operation,
            duration_ms = entry.duration_ms,
            success = entry.success,
            backend = %entry.backend,
            "telemetry entry recorded"
        );

        self.total_ops.fetch_add(1, Ordering::Relaxed);
        if !entry.success {
            self.total_errors.fetch_add(1, Ordering::Relaxed);
        }

        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(entry);

        // Trim to half capacity when the buffer overflows.
        if guard.len() > MAX_ENTRIES {
            let drain_to = guard.len() - MAX_ENTRIES / 2;
            guard.drain(0..drain_to);
        }
    }

    /// Compute a [`TelemetrySummary`] from the current ring-buffer snapshot.
    ///
    /// Statistics are computed in O(n log n) time over the buffered entries
    /// only (not the full lifetime count).  The lifetime `total_operations`
    /// and `total_errors` counters are accurate regardless of ring-buffer
    /// eviction.
    pub fn summary(&self) -> TelemetrySummary {
        let total_operations = self.total_ops.load(Ordering::Relaxed);
        let total_errors = self.total_errors.load(Ordering::Relaxed);

        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if guard.is_empty() {
            return TelemetrySummary {
                total_operations,
                total_errors,
                avg_latency_ms: 0.0,
                p95_latency_ms: 0.0,
                operations_per_backend: HashMap::new(),
            };
        }

        // Collect latencies and compute mean.
        let mut durations: Vec<u64> = guard.iter().map(|e| e.duration_ms).collect();
        let sum: u64 = durations.iter().sum();
        let avg_latency_ms = sum as f64 / durations.len() as f64;

        // Sort for percentile calculation (ascending).
        durations.sort_unstable();
        let p95_index = percentile_index(durations.len(), 95);
        let p95_latency_ms = durations[p95_index] as f64;

        // Operations per backend.
        let mut operations_per_backend: HashMap<String, u64> = HashMap::new();
        for entry in guard.iter() {
            *operations_per_backend
                .entry(entry.backend.clone())
                .or_insert(0) += 1;
        }

        TelemetrySummary {
            total_operations,
            total_errors,
            avg_latency_ms,
            p95_latency_ms,
            operations_per_backend,
        }
    }

    /// Return up to `limit` most-recent entries from the ring-buffer.
    ///
    /// Entries are returned in insertion order (oldest first within the
    /// returned slice).
    pub fn recent(&self, limit: usize) -> Vec<OperationTelemetry> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let start = guard.len().saturating_sub(limit);
        guard[start..].to_vec()
    }

    /// Return the lifetime total operation count (never decreases, even after
    /// ring-buffer eviction).
    pub fn total_operations(&self) -> u64 {
        self.total_ops.load(Ordering::Relaxed)
    }

    /// Return the lifetime total error count.
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }

    /// Return the number of entries currently held in the ring-buffer.
    pub fn buffered_count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Default for TelemetryCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TimedOperation — RAII guard for convenient timing
// ---------------------------------------------------------------------------

/// RAII guard that records an [`OperationTelemetry`] entry on drop.
///
/// Create via [`TelemetryCollector::time`] (or construct directly).  The
/// operation is recorded as successful unless [`TimedOperation::fail`] is
/// called before drop.
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
/// use glassbridge_desktop::telemetry::{TelemetryCollector, TimedOperation};
///
/// let collector = Arc::new(TelemetryCollector::new());
/// {
///     let _t = TimedOperation::start(Arc::clone(&collector), "list_devices", "process");
///     // … do work …
/// } // records entry on drop
/// assert_eq!(collector.total_operations(), 1);
/// ```
pub struct TimedOperation {
    collector: Arc<TelemetryCollector>,
    operation: String,
    backend: String,
    started: Instant,
    success: bool,
}

impl TimedOperation {
    /// Begin timing an operation.
    pub fn start(
        collector: Arc<TelemetryCollector>,
        operation: impl Into<String>,
        backend: impl Into<String>,
    ) -> Self {
        Self {
            collector,
            operation: operation.into(),
            backend: backend.into(),
            started: Instant::now(),
            success: true,
        }
    }

    /// Mark this operation as failed.  The entry will be recorded with
    /// `success = false` when this guard is dropped.
    pub fn fail(&mut self) {
        self.success = false;
    }
}

impl Drop for TimedOperation {
    fn drop(&mut self) {
        let duration_ms = self.started.elapsed().as_millis() as u64;
        self.collector.record(OperationTelemetry {
            operation: self.operation.clone(),
            duration_ms,
            success: self.success,
            backend: self.backend.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the index in a sorted slice of `len` elements corresponding to the
/// `pct`-th percentile (0–100), using the "nearest rank" method.
///
/// Always returns a valid index in `[0, len)`.
fn percentile_index(len: usize, pct: u8) -> usize {
    debug_assert!(len > 0, "len must be > 0 before calling percentile_index");
    let pct = pct.min(100) as usize;
    let index = (pct * len).saturating_sub(1) / 100;
    index.min(len - 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(op: &str, duration_ms: u64, success: bool, backend: &str) -> OperationTelemetry {
        OperationTelemetry {
            operation: op.into(),
            duration_ms,
            success,
            backend: backend.into(),
        }
    }

    // -- TelemetryCollector basics -------------------------------------------

    #[test]
    fn new_collector_has_zero_counts() {
        let c = TelemetryCollector::new();
        assert_eq!(c.total_operations(), 0);
        assert_eq!(c.total_errors(), 0);
        assert_eq!(c.buffered_count(), 0);
    }

    #[test]
    fn record_increments_total_ops() {
        let c = TelemetryCollector::new();
        c.record(make_entry("op", 10, true, "process"));
        c.record(make_entry("op", 20, true, "process"));
        assert_eq!(c.total_operations(), 2);
        assert_eq!(c.total_errors(), 0);
    }

    #[test]
    fn record_increments_errors_on_failure() {
        let c = TelemetryCollector::new();
        c.record(make_entry("op", 10, true, "process"));
        c.record(make_entry("op", 15, false, "process"));
        c.record(make_entry("op", 20, false, "process"));
        assert_eq!(c.total_operations(), 3);
        assert_eq!(c.total_errors(), 2);
    }

    // -- summary() -----------------------------------------------------------

    #[test]
    fn summary_empty_collector_returns_zeros() {
        let c = TelemetryCollector::new();
        let s = c.summary();
        assert_eq!(s.total_operations, 0);
        assert_eq!(s.total_errors, 0);
        assert_eq!(s.avg_latency_ms, 0.0);
        assert_eq!(s.p95_latency_ms, 0.0);
        assert!(s.operations_per_backend.is_empty());
    }

    #[test]
    fn summary_single_entry() {
        let c = TelemetryCollector::new();
        c.record(make_entry("list_devices", 42, true, "process"));
        let s = c.summary();
        assert_eq!(s.total_operations, 1);
        assert_eq!(s.total_errors, 0);
        assert!((s.avg_latency_ms - 42.0).abs() < f64::EPSILON);
        assert!((s.p95_latency_ms - 42.0).abs() < f64::EPSILON);
        assert_eq!(s.operations_per_backend.get("process"), Some(&1));
    }

    #[test]
    fn summary_avg_latency_is_mean_of_durations() {
        let c = TelemetryCollector::new();
        c.record(make_entry("a", 10, true, "process"));
        c.record(make_entry("b", 20, true, "process"));
        c.record(make_entry("c", 30, true, "process"));
        let s = c.summary();
        assert!((s.avg_latency_ms - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_p95_with_20_entries() {
        let c = TelemetryCollector::new();
        for i in 1u64..=20 {
            c.record(make_entry("op", i * 10, true, "process"));
        }
        let s = c.summary();
        // Sorted: [10, 20, …, 200].  p95 nearest-rank = index 18 → 190.
        assert!((s.p95_latency_ms - 190.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_operations_per_backend_groups_correctly() {
        let c = TelemetryCollector::new();
        c.record(make_entry("a", 1, true, "process"));
        c.record(make_entry("b", 2, true, "process"));
        c.record(make_entry("c", 3, true, "native"));
        let s = c.summary();
        assert_eq!(s.operations_per_backend.get("process"), Some(&2));
        assert_eq!(s.operations_per_backend.get("native"), Some(&1));
    }

    // -- recent() ------------------------------------------------------------

    #[test]
    fn recent_returns_last_n_entries() {
        let c = TelemetryCollector::new();
        for i in 1u64..=10 {
            c.record(make_entry(&format!("op{i}"), i * 5, true, "process"));
        }
        let last3 = c.recent(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].operation, "op8");
        assert_eq!(last3[1].operation, "op9");
        assert_eq!(last3[2].operation, "op10");
    }

    #[test]
    fn recent_limit_larger_than_buffer_returns_all() {
        let c = TelemetryCollector::new();
        c.record(make_entry("only", 1, true, "process"));
        let entries = c.recent(100);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn recent_empty_collector_returns_empty() {
        let c = TelemetryCollector::new();
        assert!(c.recent(10).is_empty());
    }

    // -- ring-buffer eviction ------------------------------------------------

    #[test]
    fn buffer_does_not_exceed_max_entries_plus_one() {
        let c = TelemetryCollector::new();
        // Insert MAX_ENTRIES + 10 to trigger eviction.
        for i in 0..(MAX_ENTRIES + 10) as u64 {
            c.record(make_entry("op", i, true, "process"));
        }
        // After eviction the buffer should be at most MAX_ENTRIES.
        assert!(
            c.buffered_count() <= MAX_ENTRIES,
            "buffer {} exceeds MAX_ENTRIES {}",
            c.buffered_count(),
            MAX_ENTRIES
        );
        // Total ops counter must still be accurate.
        assert_eq!(c.total_operations(), (MAX_ENTRIES + 10) as u64);
    }

    // -- TimedOperation ------------------------------------------------------

    #[test]
    fn timed_operation_records_on_drop() {
        let c = Arc::new(TelemetryCollector::new());
        {
            let _t = TimedOperation::start(Arc::clone(&c), "test_op", "process");
        }
        assert_eq!(c.total_operations(), 1);
        assert_eq!(c.total_errors(), 0);
    }

    #[test]
    fn timed_operation_fail_records_error() {
        let c = Arc::new(TelemetryCollector::new());
        {
            let mut t = TimedOperation::start(Arc::clone(&c), "failing_op", "process");
            t.fail();
        }
        assert_eq!(c.total_operations(), 1);
        assert_eq!(c.total_errors(), 1);
    }

    #[test]
    fn timed_operation_duration_is_nonzero_for_real_work() {
        let c = Arc::new(TelemetryCollector::new());
        {
            let _t = TimedOperation::start(Arc::clone(&c), "work", "native");
            // Spin briefly so the duration is non-trivial.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let entries = c.recent(1);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].duration_ms >= 1,
            "expected >= 1 ms, got {}",
            entries[0].duration_ms
        );
    }

    // -- percentile_index ----------------------------------------------------

    #[test]
    fn percentile_index_single_element() {
        assert_eq!(percentile_index(1, 50), 0);
        assert_eq!(percentile_index(1, 95), 0);
        assert_eq!(percentile_index(1, 100), 0);
    }

    #[test]
    fn percentile_index_100_entries_p95_is_94() {
        // nearest-rank: ceil(95/100 * 100) - 1 = 94
        assert_eq!(percentile_index(100, 95), 94);
    }

    #[test]
    fn percentile_index_100_entries_p50_is_49() {
        assert_eq!(percentile_index(100, 50), 49);
    }

    #[test]
    fn percentile_index_clamped_to_last_element() {
        // pct=100 must not exceed len-1
        let last = percentile_index(5, 100);
        assert!(last < 5, "index {last} must be < 5");
    }

    // -- Serialization -------------------------------------------------------

    #[test]
    fn operation_telemetry_serialises_camel_case() {
        let entry = make_entry("list_devices", 42, true, "process");
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("operation").is_some());
        assert!(
            json.get("durationMs").is_some(),
            "must be camelCase durationMs"
        );
        assert!(json.get("duration_ms").is_none());
        assert!(json.get("success").is_some());
        assert!(json.get("backend").is_some());
    }

    #[test]
    fn telemetry_summary_serialises_camel_case() {
        let c = TelemetryCollector::new();
        c.record(make_entry("op", 10, true, "process"));
        let summary = c.summary();
        let json = serde_json::to_value(&summary).unwrap();
        assert!(json.get("totalOperations").is_some());
        assert!(json.get("totalErrors").is_some());
        assert!(json.get("avgLatencyMs").is_some());
        assert!(json.get("p95LatencyMs").is_some());
        assert!(json.get("operationsPerBackend").is_some());
    }
}
