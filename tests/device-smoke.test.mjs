/**
 * device-smoke.test.mjs
 *
 * Hardware-in-the-loop smoke tests.  Every test in this file requires a
 * physical Android device.  When no device is connected the entire suite
 * skips gracefully — nothing fails.
 *
 * Run explicitly:
 *   npm run test:device
 *
 * Force hard-failure when device is expected:
 *   GLASSBRIDGE_REQUIRE_DEVICE=1 npm run test:device
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import test from "node:test";
import { resolveDeviceContext, skipWithReason } from "./device-context.mjs";
import { hasDevice } from "./test-helpers.mjs";

// ---------------------------------------------------------------------------
// Pre-flight: probe adb directly before spinning up the backend server.
// This gives a fast, clear skip when the developer has no device attached
// rather than waiting 20 s for the health-check to time out.
// ---------------------------------------------------------------------------

const adbProbe = await hasDevice();

if (!adbProbe.available && process.env.GLASSBRIDGE_REQUIRE_DEVICE !== "1") {
  console.log(
    "\n  Skipping device tests: no authorized Android device connected\n",
  );
  // Exit cleanly — node:test treats process.exit(0) as a passed (skipped) run.
  process.exit(0);
}

const port = Number(process.env.TEST_SERVER_PORT ?? (await reservePort()));
const baseUrl = `http://127.0.0.1:${port}`;
let serverProcess;
let deviceCtx; // Set in before(); may be { skipped: true } when no device
let serverLogs = "";

test.before(async () => {
  serverProcess = spawn("node", ["apps/server/dist/index.js"], {
    cwd: process.cwd(),
    env: { ...process.env, PORT: String(port), HOST: "127.0.0.1" },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });

  serverProcess.stdout.on("data", (chunk) => {
    serverLogs += chunk.toString();
  });
  serverProcess.stderr.on("data", (chunk) => {
    serverLogs += chunk.toString();
  });

  await waitForHealth();

  deviceCtx = await resolveDeviceContext(baseUrl, () => serverLogs);

  if (deviceCtx.skipped) {
    console.log(`\n  SKIP REASON: ${deviceCtx.reason}\n`);
  } else {
    console.log(
      `\n  Device ready: ${deviceCtx.device.model ?? deviceCtx.device.serial}\n`,
    );
  }
});

test.after(() => {
  serverProcess?.kill();
});

// ---------------------------------------------------------------------------
// Device-dependent tests — each guards itself with the shared context.
// ---------------------------------------------------------------------------

test("reports the connected target device", async (t) => {
  if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);

  const { device } = deviceCtx;
  const shellPayload = await postJson(
    `/api/devices/${encodeURIComponent(device.serial)}/shell`,
    { command: "getprop ro.product.model" },
  );

  // Accept any non-empty model string — we already selected the best match.
  assert.ok(
    shellPayload.stdout.trim().length > 0,
    `Expected non-empty model from getprop, got: ${shellPayload.stdout}`,
  );
});

test("lists app packages for the connected target device", async (t) => {
  if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);

  const { device } = deviceCtx;
  const packagesPayload = await getJson(
    `/api/devices/${encodeURIComponent(device.serial)}/apps`,
  );
  assert.ok(Array.isArray(packagesPayload.packages), "Expected packages array");
  assert.ok(
    packagesPayload.packages.length > 0,
    "Expected at least one third-party package",
  );
});

test("can approve the connected target device into the local registry", async (t) => {
  if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);

  const { device } = deviceCtx;
  const alias = `validated-${device.serial}`;
  const approvalPayload = await postJson(
    `/api/approved-devices/${encodeURIComponent(device.serial)}/approve`,
    { alias, notes: "Recorded by automated validation" },
  );

  assert.equal(approvalPayload.ok, true);
  const approvedPayload = await getJson("/api/approved-devices");
  const approvedDevice = approvedPayload.devices.find(
    (d) => d.serial === device.serial,
  );
  assert.ok(approvedDevice, "Expected approved device entry");
  assert.equal(approvedDevice.alias, alias);
});

test("accepts a quick control action", async (t) => {
  if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);

  const { device } = deviceCtx;
  const controlPayload = await postJson(
    `/api/devices/${encodeURIComponent(device.serial)}/control`,
    { action: "wake" },
  );
  assert.equal(controlPayload.exitCode, 0);
});

test("returns a valid PNG screenshot for the connected target device", async (t) => {
  if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);

  const { device } = deviceCtx;
  const response = await fetch(
    `${baseUrl}/api/devices/${encodeURIComponent(device.serial)}/screenshot`,
  );
  const screenshot = new Uint8Array(await response.arrayBuffer());

  assert.equal(response.ok, true);
  assert.equal(response.headers.get("content-type"), "image/png");
  assert.ok(screenshot.byteLength > 1024, "Expected non-trivial PNG output");
  assert.deepEqual(
    Array.from(screenshot.slice(0, 8)),
    [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
  );
});

// ---------------------------------------------------------------------------
// Server-only smoke (no device required — verifies the server runs at all)
// ---------------------------------------------------------------------------

test("serves the built frontend shell", async () => {
  const response = await fetch(baseUrl);
  const html = await response.text();
  assert.equal(response.ok, true);
  assert.match(html, /Android Control Console/i);
});

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function waitForHealth() {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/api/health`);
      if (response.ok) return;
    } catch {
      // Server still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`Server did not become healthy in time.\n${serverLogs}`);
}

async function getJson(path) {
  const response = await fetch(`${baseUrl}${path}`);
  const payload = await response.json();
  assert.equal(response.ok, true, payload?.error ?? JSON.stringify(payload));
  return payload;
}

async function postJson(path, body) {
  const response = await fetch(`${baseUrl}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const payload = await response.json();
  assert.equal(response.ok, true, payload?.error ?? JSON.stringify(payload));
  return payload;
}

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("Failed to reserve a TCP port"));
        return;
      }
      const { port: reservedPort } = address;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(String(reservedPort));
      });
    });
  });
}
