// @deprecated — Replaced by apps/web/src-tauri/src/registry.rs (native Rust SQLite registry).
// Retained for browser-mode development only (`npm run dev`).
import { mkdirSync } from "node:fs";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import type { DeviceRecord } from "./adb.js";
import { dataRoot } from "./config.js";

export type RegistryRecord = {
  serial: string;
  alias: string | null;
  model: string | null;
  product: string | null;
  device: string | null;
  approvalStatus: string;
  approvalSource: string | null;
  notes: string | null;
  firstSeenAt: string;
  lastSeenAt: string;
  lastTransportId: string | null;
};

mkdirSync(dataRoot, { recursive: true });

const db = new DatabaseSync(path.join(dataRoot, "device-registry.db"));
db.exec("PRAGMA journal_mode = WAL;");
db.exec(`
  CREATE TABLE IF NOT EXISTS device_registry (
    serial TEXT PRIMARY KEY,
    alias TEXT,
    model TEXT,
    product TEXT,
    device TEXT,
    approval_status TEXT NOT NULL DEFAULT 'candidate',
    approval_source TEXT,
    notes TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_transport_id TEXT
  );

  CREATE TABLE IF NOT EXISTS device_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    serial TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    state TEXT NOT NULL,
    transport_id TEXT
  );
`);

const upsertSeenDevice = db.prepare(`
  INSERT INTO device_registry (
    serial, alias, model, product, device, approval_status, approval_source, notes, first_seen_at, last_seen_at, last_transport_id
  ) VALUES (
    ?, NULL, ?, ?, ?, 'candidate', NULL, NULL, ?, ?, ?
  )
  ON CONFLICT(serial) DO UPDATE SET
    model = excluded.model,
    product = excluded.product,
    device = excluded.device,
    last_seen_at = excluded.last_seen_at,
    last_transport_id = excluded.last_transport_id
`);

const insertSession = db.prepare(`
  INSERT INTO device_sessions (serial, observed_at, state, transport_id)
  VALUES (?, ?, ?, ?)
`);

const approveDeviceStatement = db.prepare(`
  UPDATE device_registry
  SET
    alias = COALESCE(?, alias),
    notes = COALESCE(?, notes),
    approval_status = 'approved',
    approval_source = ?,
    last_seen_at = ?
  WHERE serial = ?
`);

const insertApprovedDeviceStatement = db.prepare(`
  INSERT INTO device_registry (
    serial, alias, model, product, device, approval_status, approval_source, notes, first_seen_at, last_seen_at, last_transport_id
  ) VALUES (
    ?, ?, ?, ?, ?, 'approved', ?, ?, ?, ?, ?
  )
  ON CONFLICT(serial) DO UPDATE SET
    alias = COALESCE(excluded.alias, alias),
    model = COALESCE(excluded.model, model),
    product = COALESCE(excluded.product, product),
    device = COALESCE(excluded.device, device),
    approval_status = 'approved',
    approval_source = excluded.approval_source,
    notes = COALESCE(excluded.notes, notes),
    last_seen_at = excluded.last_seen_at,
    last_transport_id = COALESCE(excluded.last_transport_id, last_transport_id)
`);

export function syncSeenDevices(devices: DeviceRecord[]) {
  const observedAt = new Date().toISOString();
  try {
    db.exec("BEGIN");
    for (const row of devices) {
      upsertSeenDevice.run(
        row.serial,
        row.model,
        row.product,
        row.device,
        observedAt,
        observedAt,
        row.transportId,
      );
      insertSession.run(row.serial, observedAt, row.state, row.transportId);
    }
    db.exec("COMMIT");
  } catch (error) {
    db.exec("ROLLBACK");
    throw error;
  }
}

export function approveDevice(
  device: DeviceRecord,
  options?: { alias?: string; notes?: string; approvalSource?: string },
) {
  const observedAt = new Date().toISOString();
  const alias = options?.alias ?? null;
  const notes = options?.notes ?? null;
  const approvalSource = options?.approvalSource ?? "local-ui";
  insertApprovedDeviceStatement.run(
    device.serial,
    alias,
    device.model,
    device.product,
    device.device,
    approvalSource,
    notes,
    observedAt,
    observedAt,
    device.transportId,
  );
  approveDeviceStatement.run(
    alias,
    notes,
    approvalSource,
    observedAt,
    device.serial,
  );
}

export function listApprovedDevices(): RegistryRecord[] {
  return db
    .prepare(`
      SELECT
        serial,
        alias,
        model,
        product,
        device,
        approval_status AS approvalStatus,
        approval_source AS approvalSource,
        notes,
        first_seen_at AS firstSeenAt,
        last_seen_at AS lastSeenAt,
        last_transport_id AS lastTransportId
      FROM device_registry
      WHERE approval_status = 'approved'
      ORDER BY COALESCE(alias, model, serial)
    `)
    .all() as RegistryRecord[];
}

export function getRegistryMap() {
  const rows = db
    .prepare(`
      SELECT
        serial,
        alias,
        approval_status AS approvalStatus,
        notes,
        first_seen_at AS firstSeenAt,
        last_seen_at AS lastSeenAt
      FROM device_registry
    `)
    .all() as Array<{
    serial: string;
    alias: string | null;
    approvalStatus: string;
    notes: string | null;
    firstSeenAt: string;
    lastSeenAt: string;
  }>;

  return new Map(rows.map((row) => [row.serial, row]));
}

export function getRegistrySummary() {
  const [{ approvedCount, knownCount }] = db
    .prepare(`
      SELECT
        SUM(CASE WHEN approval_status = 'approved' THEN 1 ELSE 0 END) AS approvedCount,
        COUNT(*) AS knownCount
      FROM device_registry
    `)
    .all() as Array<{ approvedCount: number | null; knownCount: number }>;

  return {
    approvedCount: approvedCount ?? 0,
    knownCount,
  };
}
