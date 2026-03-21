/**
 * frontend-backend.integration.test.ts
 *
 * Boots a real backend server, seeds data from its live API responses, then
 * drives the frontend against those payloads inside a jsdom environment.
 *
 * The base health / device-list / frontend-render tests run without hardware.
 * Device-specific assertions (serial visible, screenshot present) activate
 * automatically when a device IS attached.
 *
 * To hard-fail instead of skip when no device is detected:
 *   GLASSBRIDGE_REQUIRE_DEVICE=1 npm run test:integration
 */

import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { getJson, hasDevice, startServer } from "./test-helpers.mjs";

// ---------------------------------------------------------------------------
// Module-level device probe — runs once before any test is collected.
// Vitest sees test.skip() calls during collection and marks them properly,
// which guarantees exit code 0 for a fully-skipped suite.
// ---------------------------------------------------------------------------

const deviceProbe = await hasDevice();
const requireDevice = process.env.GLASSBRIDGE_REQUIRE_DEVICE === "1";

if (!deviceProbe.available && requireDevice) {
  throw new Error(
    "GLASSBRIDGE_REQUIRE_DEVICE=1 but no authorized Android device found. " +
      "Connect a device with USB debugging enabled and retry.",
  );
}

/**
 * Conditionally skips a test when no device is available.
 * Passes through when a device IS present or when the test is device-agnostic.
 */
function maybeSkipDeviceTest(
  label: string,
  fn: () => Promise<void>,
  timeout?: number,
) {
  if (!deviceProbe.available) {
    test.skip(label);
  } else if (timeout !== undefined) {
    test(label, fn, timeout);
  } else {
    test(label, fn);
  }
}

class MockWebSocket {
  onmessage = null;
  onclose = null;
}

let server: Awaited<ReturnType<typeof startServer>>;
const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;

if (!deviceProbe.available) {
  console.log(
    "\n  Skipping device tests: no authorized Android device connected\n",
  );
}

beforeEach(async () => {
  server = await startServer();
  document.body.innerHTML = `<div id="app"></div>`;
  URL.createObjectURL = vi.fn(() => "blob:integration-screen-preview");
  URL.revokeObjectURL = vi.fn();

  // Seed live payloads from the running backend.
  const tools = await getJson(server.baseUrl, "/api/tools/status");
  const devices = await getJson(server.baseUrl, "/api/devices");
  const help = await getJson(server.baseUrl, "/api/help/topics");
  const approved = await getJson(server.baseUrl, "/api/approved-devices");

  // Screenshot and package list are optional — only attempt if a device exists.
  const selectedSerial = devices.devices[0]?.serial;
  const packages = selectedSerial
    ? await getJson(
        server.baseUrl,
        `/api/devices/${encodeURIComponent(selectedSerial)}/apps`,
      )
    : { packages: [] };

  const screenshotResponse = selectedSerial
    ? await fetch(
        `${server.baseUrl}/api/devices/${encodeURIComponent(selectedSerial)}/screenshot`,
      )
    : null;
  const screenshotBytes = screenshotResponse
    ? new Uint8Array(await screenshotResponse.arrayBuffer())
    : null;

  const livePayloads = new Map<string, unknown>([
    ["/api/health", { ok: true }],
    ["/api/tools/status", tools],
    ["/api/devices", devices],
    ["/api/help/topics", help],
    ["/api/approved-devices", approved],
    [`/api/devices/${encodeURIComponent(selectedSerial ?? "")}/apps`, packages],
  ]);

  const fetchFromLivePayloads = async (input: string, init?: RequestInit) => {
    if ((init?.method ?? "GET") !== "GET") {
      return new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }

    const normalizedPath = new URL(input, server.baseUrl).pathname;

    if (normalizedPath.endsWith("/screenshot")) {
      return new Response(screenshotBytes, {
        status: 200,
        headers: { "Content-Type": "image/png" },
      });
    }

    const payload = livePayloads.get(normalizedPath);
    if (!payload) {
      throw new Error(`Unhandled integration fetch path: ${input}`);
    }

    return new Response(JSON.stringify(payload), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  vi.stubGlobal("fetch", fetchFromLivePayloads);
  window.fetch = fetchFromLivePayloads as typeof window.fetch;
  vi.stubGlobal("WebSocket", MockWebSocket);
}, 20_000);

afterEach(async () => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalCreateObjectURL) {
    URL.createObjectURL = originalCreateObjectURL;
  } else {
    delete (URL as unknown as Record<string, unknown>).createObjectURL;
  }
  if (originalRevokeObjectURL) {
    URL.revokeObjectURL = originalRevokeObjectURL;
  } else {
    delete (URL as unknown as Record<string, unknown>).revokeObjectURL;
  }
  document.body.innerHTML = "";
  await server.stop();
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test("backend health endpoint is reachable", async () => {
  const health = await getJson(server.baseUrl, "/api/health");
  expect(health.ok).toBe(true);
});

test("backend returns devices list (may be empty without hardware)", async () => {
  const devices = await getJson(server.baseUrl, "/api/devices");
  expect(Array.isArray(devices.devices)).toBe(true);
});

test("frontend renders against live backend payloads (no device required)", async () => {
  vi.resetModules();
  await import("../apps/web/src/main.ts");
  await settle();

  // The UI must render without crashing regardless of device count.
  expect(document.body.textContent).toContain("Guided Help");
  expect(document.body.textContent).toContain("Approved Fleet");
}, 20_000);

maybeSkipDeviceTest(
  "frontend shows device serial and screen preview when device is connected",
  async () => {
    const devices = await getJson(server.baseUrl, "/api/devices");

    vi.resetModules();
    await import("../apps/web/src/main.ts");
    await settle();

    // When a device IS present the serial must appear.
    expect(devices.devices.length).toBeGreaterThan(0);
    expect(document.body.textContent).toContain(devices.devices[0].serial);
    const screenPreview = await waitForScreenPreview();
    expect(screenPreview?.src).toContain("blob:integration-screen-preview");
  },
  20_000,
);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function settle() {
  for (let i = 0; i < 8; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function waitForScreenPreview(timeoutMs = 12_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const el = document.querySelector<HTMLImageElement>(
      "img[alt='Device screen preview']",
    );
    if (el) return el;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return null;
}
