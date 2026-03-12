import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import test from "node:test";

const port = Number(process.env.TEST_SERVER_PORT ?? (await reservePort()));
const baseUrl = `http://127.0.0.1:${port}`;
const targetLabel = process.env.ANDROID_TARGET_MODEL ?? "Pixel 10";
let serverProcess;
let selectedDevice;
let serverLogs = "";

test.before(async () => {
  serverProcess = spawn("node", ["apps/server/dist/index.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(port),
      HOST: "127.0.0.1",
    },
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

  const toolsPayload = await getJson("/api/tools/status");
  assert.equal(
    toolsPayload.status.adb.installed,
    true,
    `adb is not installed.\n${serverLogs}`,
  );
  assert.equal(
    toolsPayload.status.scrcpy.installed,
    true,
    `scrcpy is not installed.\n${serverLogs}`,
  );

  const devicesPayload = await getJson("/api/devices");
  const devices = devicesPayload.devices.filter(
    (device) => device.state === "device",
  );
  assert.ok(
    devices.length > 0,
    `No authorized Android device found.\n${serverLogs}`,
  );

  selectedDevice =
    devices.find((device) => {
      return (
        includesNormalized(device.model ?? "", targetLabel) ||
        includesNormalized(device.product ?? "", targetLabel)
      );
    }) ?? devices[0];
});

test.after(() => {
  if (!serverProcess) {
    return;
  }
  serverProcess.kill();
});

test("reports the connected target device", async () => {
  assert.ok(selectedDevice, "No target device selected");
  const shellPayload = await postJson(
    `/api/devices/${encodeURIComponent(selectedDevice.serial)}/shell`,
    {
      command: "getprop ro.product.model",
    },
  );

  assert.equal(
    includesNormalized(shellPayload.stdout, targetLabel),
    true,
    `Expected target model match, got ${shellPayload.stdout}`,
  );
});

test("lists app packages for the connected target device", async () => {
  assert.ok(selectedDevice, "No target device selected");
  const packagesPayload = await getJson(
    `/api/devices/${encodeURIComponent(selectedDevice.serial)}/apps`,
  );
  assert.ok(Array.isArray(packagesPayload.packages), "Expected packages array");
  assert.ok(
    packagesPayload.packages.length > 0,
    "Expected at least one third-party package",
  );
});

test("can approve the connected target device into the local registry", async () => {
  assert.ok(selectedDevice, "No target device selected");
  const alias = `validated-${selectedDevice.serial}`;
  const approvalPayload = await postJson(
    `/api/approved-devices/${encodeURIComponent(selectedDevice.serial)}/approve`,
    {
      alias,
      notes: "Recorded by automated validation",
    },
  );

  assert.equal(approvalPayload.ok, true);
  const approvedPayload = await getJson("/api/approved-devices");
  const approvedDevice = approvedPayload.devices.find(
    (device) => device.serial === selectedDevice.serial,
  );
  assert.ok(approvedDevice, "Expected approved device entry");
  assert.equal(approvedDevice.alias, alias);
});

test("accepts a quick control action", async () => {
  assert.ok(selectedDevice, "No target device selected");
  const controlPayload = await postJson(
    `/api/devices/${encodeURIComponent(selectedDevice.serial)}/control`,
    {
      action: "wake",
    },
  );

  assert.equal(controlPayload.exitCode, 0);
});

test("serves the built frontend shell", async () => {
  const response = await fetch(baseUrl);
  const html = await response.text();

  assert.equal(response.ok, true);
  assert.match(html, /Android Control Console/i);
});

async function waitForHealth() {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/api/health`);
      if (response.ok) {
        return;
      }
    } catch {
      // Server is still starting.
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
    headers: {
      "Content-Type": "application/json",
    },
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

function includesNormalized(actual, expected) {
  const normalize = (value) =>
    value
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, " ")
      .trim();
  return normalize(actual).includes(normalize(expected));
}
