// DEPRECATED: apps/server/src/registry.ts — replaced by this native Rust registry
//
// Persistent device registry backed by SQLite (rusqlite, bundled).
//
// The registry tracks every ADB device ever seen, records per-observation
// sessions, and manages an approval workflow.  It is a direct port of the
// Node.js `DatabaseSync`-based registry; the schema and SQL are identical so
// existing `device-registry.db` files from the Node.js server are
// transparently compatible.
//
// # Thread safety
//
// `Registry` owns a `rusqlite::Connection`, which is `!Send`.  Callers that
// need to share the registry across threads must wrap it in
// `Arc<Mutex<Registry>>`.  The `Mutex` boundary is the correct place to
// enforce single-writer access; no internal locking is performed here.

use std::{
    collections::HashMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by registry operations.
///
/// # Examples
///
/// ```rust,ignore
/// match registry.get_registry_summary() {
///     Ok(s) => println!("approved={}", s.approved_count),
///     Err(RegistryError::Sqlite(e)) => eprintln!("db error: {e}"),
///     Err(RegistryError::Io(e))     => eprintln!("io error: {e}"),
/// }
/// ```
#[derive(Debug, Error)]
pub enum RegistryError {
    /// A rusqlite operation failed.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem operation failed (e.g. creating the data directory).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[allow(dead_code)]
impl RegistryError {
    /// Returns `true` if this is a SQLite-layer error.
    #[must_use]
    pub fn is_sqlite(&self) -> bool {
        matches!(self, Self::Sqlite(_))
    }

    /// Returns `true` if this is an I/O-layer error.
    #[must_use]
    pub fn is_io(&self) -> bool {
        matches!(self, Self::Io(_))
    }
}

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// A single row from `device_registry`, serialised with camelCase keys so
/// Tauri IPC and JSON REST responses match the TypeScript contract exactly.
///
/// # Examples
///
/// ```rust,ignore
/// let records = registry.list_approved_devices()?;
/// for r in &records {
///     println!("{} — {}", r.serial, r.approval_status);
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRecord {
    pub serial: String,
    pub alias: Option<String>,
    pub model: Option<String>,
    pub product: Option<String>,
    pub device: Option<String>,
    pub approval_status: String,
    pub approval_source: Option<String>,
    pub notes: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_transport_id: Option<String>,
}

/// Counts returned by [`Registry::get_registry_summary`].
///
/// # Examples
///
/// ```rust,ignore
/// let s = registry.get_registry_summary()?;
/// println!("{}/{} devices approved", s.approved_count, s.known_count);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrySummary {
    pub approved_count: i64,
    pub known_count: i64,
}

/// Lightweight view of a registry row, used as the value type in
/// [`Registry::get_registry_map`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryMapEntry {
    pub serial: String,
    pub alias: Option<String>,
    pub approval_status: String,
    pub notes: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

/// A device reported by `adb devices -l`.
///
/// This type is defined here so that `registry.rs` compiles standalone while
/// the full `crate::adb` module is being developed.  Once `adb.rs` exists,
/// callers should use `crate::adb::DeviceRecord` directly and remove this
/// re-export.
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub serial: String,
    /// ADB connection state (`"device"`, `"unauthorized"`, `"offline"`, …).
    pub state: String,
    pub model: Option<String>,
    pub product: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<String>,
}

/// Options accepted by [`Registry::approve_device`].
#[derive(Debug, Clone, Default)]
pub struct ApproveOptions {
    /// Human-readable label for the device shown in the UI.
    pub alias: Option<String>,
    /// Free-form operator notes.
    pub notes: Option<String>,
    /// Tag identifying who or what approved the device (default `"local-ui"`).
    pub approval_source: Option<String>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Persistent SQLite-backed device registry.
///
/// Open with [`Registry::open`], then call the methods to sync, approve, and
/// query devices.  Wrap in `Arc<Mutex<Registry>>` for shared access across
/// async tasks.
///
/// # Examples
///
/// ```rust,ignore
/// use std::path::Path;
/// let registry = Registry::open(Path::new("/data"))?;
/// registry.sync_seen_devices(&[])?;
/// let summary = registry.get_registry_summary()?;
/// assert_eq!(summary.known_count, 0);
/// ```
pub struct Registry {
    conn: Connection,
}

impl Registry {
    /// Open (or create) the registry database inside `data_root`.
    ///
    /// Creates `data_root` if it does not exist, then opens
    /// `device-registry.db` and applies the schema and WAL pragma.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Io`] if the directory cannot be created, or
    /// [`RegistryError::Sqlite`] if the database cannot be opened or the
    /// schema cannot be applied.
    pub fn open(data_root: &Path) -> Result<Self, RegistryError> {
        fs::create_dir_all(data_root)?;

        let db_path = data_root.join("device-registry.db");
        let conn = Connection::open(db_path)?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS device_registry (
                serial              TEXT PRIMARY KEY,
                alias               TEXT,
                model               TEXT,
                product             TEXT,
                device              TEXT,
                approval_status     TEXT NOT NULL DEFAULT 'candidate',
                approval_source     TEXT,
                notes               TEXT,
                first_seen_at       TEXT NOT NULL,
                last_seen_at        TEXT NOT NULL,
                last_transport_id   TEXT
            );

            CREATE TABLE IF NOT EXISTS device_sessions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                serial          TEXT NOT NULL,
                observed_at     TEXT NOT NULL,
                state           TEXT NOT NULL,
                transport_id    TEXT
            );
            ",
        )?;

        Ok(Self { conn })
    }

    /// Open an in-memory database suitable for unit tests.
    ///
    /// The schema and WAL pragma are applied identically to [`Registry::open`].
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if the in-memory database cannot be
    /// opened or the schema cannot be applied.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self, RegistryError> {
        let conn = Connection::open_in_memory()?;
        // WAL is silently ignored for :memory: databases but we apply it
        // anyway so the code path is identical to production.
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS device_registry (
                serial              TEXT PRIMARY KEY,
                alias               TEXT,
                model               TEXT,
                product             TEXT,
                device              TEXT,
                approval_status     TEXT NOT NULL DEFAULT 'candidate',
                approval_source     TEXT,
                notes               TEXT,
                first_seen_at       TEXT NOT NULL,
                last_seen_at        TEXT NOT NULL,
                last_transport_id   TEXT
            );

            CREATE TABLE IF NOT EXISTS device_sessions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                serial          TEXT NOT NULL,
                observed_at     TEXT NOT NULL,
                state           TEXT NOT NULL,
                transport_id    TEXT
            );
            ",
        )?;
        Ok(Self { conn })
    }

    // -----------------------------------------------------------------------
    // Mutations
    // -----------------------------------------------------------------------

    /// Record all currently-visible ADB devices and advance their
    /// `last_seen_at` timestamp.
    ///
    /// Each device in `devices` is upserted into `device_registry` (new rows
    /// start as `'candidate'`; existing rows have their `model`, `product`,
    /// `device`, `last_seen_at`, and `last_transport_id` refreshed).  A
    /// corresponding row is inserted into `device_sessions` for every call,
    /// building a full observation history.
    ///
    /// All inserts run inside a single transaction; on failure the transaction
    /// is rolled back and the error is returned.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if any database operation fails.
    pub fn sync_seen_devices(&self, devices: &[DeviceRecord]) -> Result<(), RegistryError> {
        let observed_at = now_rfc3339();

        // rusqlite does not expose BEGIN/COMMIT as first-class methods, so we
        // use execute_batch for the control statements and regular execute for
        // the parameterised DML.
        self.conn.execute_batch("BEGIN")?;

        let result = (|| -> Result<(), RegistryError> {
            for row in devices {
                self.conn.execute(
                    "INSERT INTO device_registry (
                         serial, alias, model, product, device,
                         approval_status, approval_source, notes,
                         first_seen_at, last_seen_at, last_transport_id
                     ) VALUES (
                         ?1, NULL, ?2, ?3, ?4,
                         'candidate', NULL, NULL,
                         ?5, ?5, ?6
                     )
                     ON CONFLICT(serial) DO UPDATE SET
                         model               = excluded.model,
                         product             = excluded.product,
                         device              = excluded.device,
                         last_seen_at        = excluded.last_seen_at,
                         last_transport_id   = excluded.last_transport_id",
                    params![
                        row.serial,
                        row.model,
                        row.product,
                        row.device,
                        observed_at,
                        row.transport_id,
                    ],
                )?;

                self.conn.execute(
                    "INSERT INTO device_sessions (serial, observed_at, state, transport_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![row.serial, observed_at, row.state, row.transport_id],
                )?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                // Best-effort rollback — ignore secondary errors.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Mark a device as `'approved'`, optionally setting an alias, notes, and
    /// approval source.
    ///
    /// Mirrors the TypeScript two-statement approach exactly:
    ///
    /// 1. **Upsert** — inserts a new row if the serial is unknown, or merges
    ///    the approved fields on conflict, preserving any existing alias,
    ///    model, product, device, notes, and last_transport_id that were
    ///    previously recorded.
    /// 2. **Update** — applies `alias`, `notes`, `approval_source`, and
    ///    `last_seen_at` to the row unconditionally (using `COALESCE` so
    ///    `None` leaves the existing column value intact).
    ///
    /// `options.approval_source` defaults to `"local-ui"` when not provided.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if any database operation fails.
    pub fn approve_device(
        &self,
        device: &DeviceRecord,
        options: ApproveOptions,
    ) -> Result<(), RegistryError> {
        let observed_at = now_rfc3339();
        let alias = options.alias.as_deref();
        let notes = options.notes.as_deref();
        let approval_source = options.approval_source.as_deref().unwrap_or("local-ui");

        // Step 1: upsert — ensures the row exists with approved status and
        // merges metadata, preferring existing non-null values via COALESCE.
        self.conn.execute(
            "INSERT INTO device_registry (
                 serial, alias, model, product, device,
                 approval_status, approval_source, notes,
                 first_seen_at, last_seen_at, last_transport_id
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5,
                 'approved', ?6, ?7,
                 ?8, ?8, ?9
             )
             ON CONFLICT(serial) DO UPDATE SET
                 alias               = COALESCE(excluded.alias,               alias),
                 model               = COALESCE(excluded.model,               model),
                 product             = COALESCE(excluded.product,             product),
                 device              = COALESCE(excluded.device,              device),
                 approval_status     = 'approved',
                 approval_source     = excluded.approval_source,
                 notes               = COALESCE(excluded.notes,               notes),
                 last_seen_at        = excluded.last_seen_at,
                 last_transport_id   = COALESCE(excluded.last_transport_id,   last_transport_id)",
            params![
                device.serial,
                alias,
                device.model,
                device.product,
                device.device,
                approval_source,
                notes,
                observed_at,
                device.transport_id,
            ],
        )?;

        // Step 2: explicit update — sets alias/notes/source on the row even
        // when the upsert above hit the DO NOTHING / COALESCE path for alias
        // and notes.  COALESCE(?, col) means: use the supplied value when it
        // is non-NULL, otherwise keep the existing column value.
        self.conn.execute(
            "UPDATE device_registry
             SET
                 alias           = COALESCE(?1, alias),
                 notes           = COALESCE(?2, notes),
                 approval_status = 'approved',
                 approval_source = ?3,
                 last_seen_at    = ?4
             WHERE serial = ?5",
            params![alias, notes, approval_source, observed_at, device.serial,],
        )?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Return all `'approved'` devices, ordered by `alias` then `model` then
    /// `serial` (matching the TypeScript `COALESCE(alias, model, serial)` sort).
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if the query fails.
    pub fn list_approved_devices(&self) -> Result<Vec<RegistryRecord>, RegistryError> {
        let mut stmt = self.conn.prepare(
            "SELECT
                 serial,
                 alias,
                 model,
                 product,
                 device,
                 approval_status,
                 approval_source,
                 notes,
                 first_seen_at,
                 last_seen_at,
                 last_transport_id
             FROM device_registry
             WHERE approval_status = 'approved'
             ORDER BY COALESCE(alias, model, serial)",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(RegistryRecord {
                serial: row.get(0)?,
                alias: row.get(1)?,
                model: row.get(2)?,
                product: row.get(3)?,
                device: row.get(4)?,
                approval_status: row.get(5)?,
                approval_source: row.get(6)?,
                notes: row.get(7)?,
                first_seen_at: row.get(8)?,
                last_seen_at: row.get(9)?,
                last_transport_id: row.get(10)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RegistryError::from)
    }

    /// Return a map of `serial → RegistryMapEntry` covering every device in
    /// the registry, regardless of approval status.
    ///
    /// Useful for fast per-serial lookups without loading the full row.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if the query fails.
    pub fn get_registry_map(&self) -> Result<HashMap<String, RegistryMapEntry>, RegistryError> {
        let mut stmt = self.conn.prepare(
            "SELECT
                 serial,
                 alias,
                 approval_status,
                 notes,
                 first_seen_at,
                 last_seen_at
             FROM device_registry",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(RegistryMapEntry {
                serial: row.get(0)?,
                alias: row.get(1)?,
                approval_status: row.get(2)?,
                notes: row.get(3)?,
                first_seen_at: row.get(4)?,
                last_seen_at: row.get(5)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RegistryError::from)
            .map(|vec| {
                vec.into_iter()
                    .map(|entry| (entry.serial.clone(), entry))
                    .collect()
            })
    }

    /// Return aggregate counts of approved and total known devices.
    ///
    /// When the registry is empty, both counts are `0`.  `approved_count`
    /// counts only rows where `approval_status = 'approved'`; `known_count`
    /// is the total row count.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Sqlite`] if the query fails.
    pub fn get_registry_summary(&self) -> Result<RegistrySummary, RegistryError> {
        let mut stmt = self.conn.prepare(
            "SELECT
                 SUM(CASE WHEN approval_status = 'approved' THEN 1 ELSE 0 END),
                 COUNT(*)
             FROM device_registry",
        )?;

        let (approved_count, known_count) = stmt.query_row([], |row| {
            // SUM returns NULL when the table is empty; treat that as 0.
            let approved: Option<i64> = row.get(0)?;
            let known: i64 = row.get(1)?;
            Ok((approved.unwrap_or(0), known))
        })?;

        Ok(RegistrySummary {
            approved_count,
            known_count,
        })
    }
}

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

/// Return the current UTC time as an RFC 3339 / ISO 8601 string, matching
/// the output of `new Date().toISOString()` in Node.js.
///
/// Format: `YYYY-MM-DDTHH:MM:SS.mmmZ`
fn now_rfc3339() -> String {
    let duration_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let secs = duration_since_epoch.as_secs();
    let millis = duration_since_epoch.subsec_millis();

    // Decompose Unix timestamp into calendar fields (Gregorian calendar,
    // UTC only — no DST, no timezone offset).
    let (year, month, day, hour, min, sec) = unix_secs_to_utc(secs);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

/// Convert a Unix timestamp (seconds since 1970-01-01T00:00:00Z) into
/// (year, month, day, hour, minute, second) in UTC.
///
/// Algorithm: Gregorian calendar proleptic calculation without any external
/// crate.  Handles all dates the registry will realistically encounter
/// (year 2000 onward).
fn unix_secs_to_utc(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec_of_day = (secs % 86_400) as u32;
    let days_since_epoch = (secs / 86_400) as u32;

    let hour = sec_of_day / 3_600;
    let min = (sec_of_day % 3_600) / 60;
    let sec = sec_of_day % 60;

    // Gregorian calendar: shift epoch from 1970-01-01 to 2000-03-01 to
    // simplify leap-year arithmetic.  2000-03-01 is day 11017 after the
    // Unix epoch (1970-01-01).
    // Reference: https://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month of year (March=0, …, Feb=11)
    let d = doy - (153 * mp + 2) / 5 + 1; // day of month [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min, sec)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // -- timestamp helper ----------------------------------------------------

    #[test]
    fn now_rfc3339_has_correct_shape() {
        let ts = now_rfc3339();
        // Must match YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24, "timestamp length should be 24: {ts}");
        assert!(
            ts.ends_with('Z'),
            "timestamp must be UTC (ends with Z): {ts}"
        );
        assert_eq!(&ts[10..11], "T", "separator at index 10 must be T: {ts}");
        assert_eq!(&ts[23..], "Z", "last char must be Z: {ts}");
    }

    #[test]
    fn unix_secs_to_utc_known_values() {
        // 2024-03-15T11:34:56Z = 1_710_502_496 seconds since epoch
        let (y, mo, d, h, mi, s) = unix_secs_to_utc(1_710_502_496);
        assert_eq!((y, mo, d), (2024, 3, 15));
        assert_eq!((h, mi, s), (11, 34, 56));
    }

    #[test]
    fn unix_secs_to_utc_epoch() {
        let (y, mo, d, h, mi, s) = unix_secs_to_utc(0);
        assert_eq!((y, mo, d), (1970, 1, 1));
        assert_eq!((h, mi, s), (0, 0, 0));
    }

    #[test]
    fn unix_secs_to_utc_leap_day_2000() {
        // 2000-02-29T00:00:00Z = 951_782_400
        let (y, mo, d, h, mi, s) = unix_secs_to_utc(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
        assert_eq!((h, mi, s), (0, 0, 0));
    }

    // -- Registry::open / schema --------------------------------------------

    #[test]
    fn open_in_memory_succeeds() {
        Registry::open_in_memory().expect("in-memory registry should open");
    }

    // -- sync_seen_devices ---------------------------------------------------

    #[test]
    fn sync_empty_slice_is_noop() {
        let reg = Registry::open_in_memory().unwrap();
        reg.sync_seen_devices(&[]).unwrap();
        let summary = reg.get_registry_summary().unwrap();
        assert_eq!(summary.known_count, 0);
        assert_eq!(summary.approved_count, 0);
    }

    #[test]
    fn sync_inserts_new_device_as_candidate() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(&[device]).unwrap();

        let summary = reg.get_registry_summary().unwrap();
        assert_eq!(summary.known_count, 1);
        assert_eq!(summary.approved_count, 0, "new device must be candidate");
    }

    #[test]
    fn sync_updates_existing_device_preserves_approval_status() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(std::slice::from_ref(&device))
            .unwrap();

        // Approve then re-sync — approval status must be preserved.
        reg.approve_device(&device, ApproveOptions::default())
            .unwrap();
        reg.sync_seen_devices(&[device]).unwrap();

        let summary = reg.get_registry_summary().unwrap();
        assert_eq!(summary.approved_count, 1, "approval must survive re-sync");
    }

    #[test]
    fn sync_creates_session_rows() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(std::slice::from_ref(&device))
            .unwrap();
        reg.sync_seen_devices(&[device]).unwrap();

        let count: i64 = reg
            .conn
            .query_row("SELECT COUNT(*) FROM device_sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "two sync calls must produce two session rows");
    }

    #[test]
    fn sync_multiple_devices_inserts_all() {
        let reg = Registry::open_in_memory().unwrap();
        let d1 = pixel_device();
        let d2 = DeviceRecord {
            serial: "emulator-5554".to_owned(),
            state: "device".to_owned(),
            model: Some("sdk_gphone64_x86_64".to_owned()),
            product: Some("sdk_gphone64_x86_64".to_owned()),
            device: Some("emu64xa".to_owned()),
            transport_id: Some("2".to_owned()),
        };
        reg.sync_seen_devices(&[d1, d2]).unwrap();

        let summary = reg.get_registry_summary().unwrap();
        assert_eq!(summary.known_count, 2);
    }

    // -- approve_device ------------------------------------------------------

    #[test]
    fn approve_unknown_device_creates_approved_row() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.approve_device(
            &device,
            ApproveOptions {
                alias: Some("My Pixel".to_owned()),
                notes: Some("Test phone".to_owned()),
                approval_source: Some("test-suite".to_owned()),
            },
        )
        .unwrap();

        let approved = reg.list_approved_devices().unwrap();
        assert_eq!(approved.len(), 1);
        let rec = &approved[0];
        assert_eq!(rec.serial, device.serial);
        assert_eq!(rec.alias.as_deref(), Some("My Pixel"));
        assert_eq!(rec.notes.as_deref(), Some("Test phone"));
        assert_eq!(rec.approval_source.as_deref(), Some("test-suite"));
        assert_eq!(rec.approval_status, "approved");
    }

    #[test]
    fn approve_defaults_approval_source_to_local_ui() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.approve_device(&device, ApproveOptions::default())
            .unwrap();

        let approved = reg.list_approved_devices().unwrap();
        assert_eq!(
            approved[0].approval_source.as_deref(),
            Some("local-ui"),
            "approval_source must default to 'local-ui'"
        );
    }

    #[test]
    fn approve_existing_candidate_upgrades_status() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(std::slice::from_ref(&device))
            .unwrap();
        reg.approve_device(&device, ApproveOptions::default())
            .unwrap();

        let summary = reg.get_registry_summary().unwrap();
        assert_eq!(summary.approved_count, 1);
        assert_eq!(summary.known_count, 1, "must not create a duplicate row");
    }

    #[test]
    fn approve_preserves_existing_alias_when_none_supplied() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();

        // First approval sets the alias.
        reg.approve_device(
            &device,
            ApproveOptions {
                alias: Some("Original Alias".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        // Second approval with no alias must NOT overwrite it.
        reg.approve_device(&device, ApproveOptions::default())
            .unwrap();

        let approved = reg.list_approved_devices().unwrap();
        assert_eq!(
            approved[0].alias.as_deref(),
            Some("Original Alias"),
            "existing alias must survive approval with alias=None"
        );
    }

    // -- list_approved_devices -----------------------------------------------

    #[test]
    fn list_approved_returns_empty_when_no_approved_devices() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(&[device]).unwrap();
        let approved = reg.list_approved_devices().unwrap();
        assert!(
            approved.is_empty(),
            "candidates must not appear in approved list"
        );
    }

    #[test]
    fn list_approved_ordering_uses_coalesce_alias_model_serial() {
        let reg = Registry::open_in_memory().unwrap();

        // Device with alias — should sort first ("Alpha").
        let d_with_alias = DeviceRecord {
            serial: "ZZZ000".to_owned(),
            state: "device".to_owned(),
            model: Some("Pixel_9".to_owned()),
            product: None,
            device: None,
            transport_id: None,
        };
        reg.approve_device(
            &d_with_alias,
            ApproveOptions {
                alias: Some("Alpha Device".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        // Device with model but no alias — should sort by model ("Beta_Model").
        let d_model_only = DeviceRecord {
            serial: "AAA111".to_owned(),
            state: "device".to_owned(),
            model: Some("Beta_Model".to_owned()),
            product: None,
            device: None,
            transport_id: None,
        };
        reg.approve_device(&d_model_only, ApproveOptions::default())
            .unwrap();

        let approved = reg.list_approved_devices().unwrap();
        assert_eq!(approved.len(), 2);
        // "Alpha Device" < "Beta_Model" lexicographically.
        assert_eq!(approved[0].serial, "ZZZ000");
        assert_eq!(approved[1].serial, "AAA111");
    }

    // -- get_registry_map ----------------------------------------------------

    #[test]
    fn registry_map_contains_all_devices() {
        let reg = Registry::open_in_memory().unwrap();
        let device = pixel_device();
        reg.sync_seen_devices(std::slice::from_ref(&device))
            .unwrap();

        let map = reg.get_registry_map().unwrap();
        assert!(map.contains_key(&device.serial));
        let entry = &map[&device.serial];
        assert_eq!(entry.approval_status, "candidate");
    }

    #[test]
    fn registry_map_is_empty_on_fresh_database() {
        let reg = Registry::open_in_memory().unwrap();
        let map = reg.get_registry_map().unwrap();
        assert!(map.is_empty());
    }

    // -- get_registry_summary ------------------------------------------------

    #[test]
    fn summary_is_zero_zero_on_fresh_database() {
        let reg = Registry::open_in_memory().unwrap();
        let s = reg.get_registry_summary().unwrap();
        assert_eq!(s.approved_count, 0);
        assert_eq!(s.known_count, 0);
    }

    #[test]
    fn summary_counts_candidates_and_approved_correctly() {
        let reg = Registry::open_in_memory().unwrap();
        let d1 = pixel_device();
        let d2 = DeviceRecord {
            serial: "emulator-5554".to_owned(),
            state: "device".to_owned(),
            model: None,
            product: None,
            device: None,
            transport_id: None,
        };

        reg.sync_seen_devices(&[d1.clone(), d2]).unwrap();
        reg.approve_device(&d1, ApproveOptions::default()).unwrap();

        let s = reg.get_registry_summary().unwrap();
        assert_eq!(s.known_count, 2);
        assert_eq!(s.approved_count, 1);
    }

    // -- RegistryError helpers -----------------------------------------------

    #[test]
    fn registry_error_is_sqlite_discriminant() {
        let err = RegistryError::Sqlite(rusqlite::Error::InvalidQuery);
        assert!(err.is_sqlite());
        assert!(!err.is_io());
    }

    #[test]
    fn registry_error_is_io_discriminant() {
        let err = RegistryError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "not found",
        ));
        assert!(err.is_io());
        assert!(!err.is_sqlite());
    }

    // -- Fixtures ------------------------------------------------------------

    fn pixel_device() -> DeviceRecord {
        DeviceRecord {
            serial: "19281FDEE000GS".to_owned(),
            state: "device".to_owned(),
            model: Some("Pixel_10_XL".to_owned()),
            product: Some("mustang".to_owned()),
            device: Some("mustang".to_owned()),
            transport_id: Some("1".to_owned()),
        }
    }
}
