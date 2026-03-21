/**
 * device-context.mjs
 *
 * Shared device-detection helper for all test suites that may run with or
 * without a physical Android device connected.
 *
 * Usage pattern:
 *   const ctx = await resolveDeviceContext(baseUrl, serverLogs);
 *   if (ctx.skipped) { console.log(ctx.reason); return; }
 *   const { device } = ctx;
 *
 * Environment variables:
 *   ANDROID_TARGET_MODEL  - partial model/product string to prefer (default: "Pixel 10")
 *   GLASSBRIDGE_REQUIRE_DEVICE - set to "1" to hard-fail instead of skip when no device
 */

export const TARGET_LABEL = process.env.ANDROID_TARGET_MODEL ?? "Pixel 10";

/**
 * Resolves device availability from a running backend server.
 *
 * Returns one of:
 *   { skipped: false, device, allDevices }   — device is ready
 *   { skipped: true,  reason, allDevices }   — no usable device; caller should skip
 *
 * Never throws for "no device" unless GLASSBRIDGE_REQUIRE_DEVICE=1.
 */
export async function resolveDeviceContext(baseUrl, readLogs = () => "") {
  const requireDevice = process.env.GLASSBRIDGE_REQUIRE_DEVICE === "1";

  // --- tool availability ---
  let toolsPayload;
  try {
    toolsPayload = await getJson(baseUrl, "/api/tools/status");
  } catch (err) {
    const reason = `Could not reach backend /api/tools/status: ${err.message}`;
    if (requireDevice) throw new Error(`${reason}\n${readLogs()}`);
    return { skipped: true, reason, allDevices: [] };
  }

  const missingTools = [];
  if (!toolsPayload.status?.adb?.installed) missingTools.push("adb");
  if (!toolsPayload.status?.scrcpy?.installed) missingTools.push("scrcpy");

  if (missingTools.length > 0) {
    const reason = `Required tools not installed: ${missingTools.join(", ")}`;
    if (requireDevice) throw new Error(`${reason}\n${readLogs()}`);
    return { skipped: true, reason, allDevices: [] };
  }

  // --- device enumeration ---
  let devicesPayload;
  try {
    devicesPayload = await getJson(baseUrl, "/api/devices");
  } catch (err) {
    const reason = `Could not reach backend /api/devices: ${err.message}`;
    if (requireDevice) throw new Error(`${reason}\n${readLogs()}`);
    return { skipped: true, reason, allDevices: [] };
  }

  const authorizedDevices = (devicesPayload.devices ?? []).filter(
    (d) => d.state === "device",
  );

  if (authorizedDevices.length === 0) {
    const reason =
      "No authorized Android device found — " +
      "connect a device with USB debugging enabled or set GLASSBRIDGE_REQUIRE_DEVICE=1 " +
      "to treat this as a hard failure.";
    if (requireDevice) throw new Error(`${reason}\n${readLogs()}`);
    return { skipped: true, reason, allDevices: [] };
  }

  // Prefer the reference device; fall back to first available.
  const device =
    authorizedDevices.find(
      (d) =>
        includesNormalized(d.model ?? "", TARGET_LABEL) ||
        includesNormalized(d.product ?? "", TARGET_LABEL),
    ) ?? authorizedDevices[0];

  return { skipped: false, device, allDevices: authorizedDevices };
}

/**
 * Wraps node:test's `skip()` so device-dependent tests emit a consistent
 * diagnostic instead of just disappearing silently.
 */
export function skipWithReason(testContext, reason) {
  // node:test uses testContext.skip(message)
  testContext.skip(`[device] ${reason}`);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

async function getJson(baseUrl, path) {
  const response = await fetch(`${baseUrl}${path}`);
  if (!response.ok) {
    const text = await response.text().catch(() => "(no body)");
    throw new Error(`HTTP ${response.status} from ${path}: ${text}`);
  }
  return response.json();
}

function includesNormalized(actual, expected) {
  const normalize = (v) =>
    v
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, " ")
      .trim();
  return normalize(actual).includes(normalize(expected));
}
