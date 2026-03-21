import { execFile, spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * Absolute path to the vendored adb binary shipped with this project.
 * The nested `platform-tools/platform-tools/` layout is intentional —
 * the outer directory is the vendor slot, the inner is the extracted SDK zip.
 */
const ADB_PATH = path.resolve(
  __dirname,
  "../vendor/platform-tools/platform-tools/adb.exe",
);

/**
 * Probes adb for connected, authorized Android devices without starting a
 * backend server.  Safe to call from any test file at module load time.
 *
 * @returns {{ available: boolean, serial: string | null, model: string | null }}
 *
 * - `available: false` when adb is absent, times-out, or no authorized device
 *   is present (offline / unauthorized devices are ignored).
 * - `available: true`  when at least one authorized device is found;
 *   `serial` and `model` reflect the first such device (prefer one whose
 *   product/model string contains the ANDROID_TARGET_MODEL env var, if set).
 */
export async function hasDevice() {
  let stdout = "";
  try {
    stdout = await new Promise((resolve, reject) => {
      execFile(
        ADB_PATH,
        ["devices", "-l"],
        { timeout: 8_000, windowsHide: true },
        (error, out, _err) => {
          // adb exits 0 even when no devices are attached; only reject on
          // spawn/timeout errors (ENOENT, ETIMEDOUT, etc.).
          if (error && error.code !== 0 && !out) {
            reject(error);
          } else {
            resolve(out ?? "");
          }
        },
      );
    });
  } catch {
    // adb not found or timed out — treat as no device.
    return { available: false, serial: null, model: null };
  }

  // Output format (one line per device after the header):
  //   <serial>  device product:<p> model:<m> device:<d> transport_id:<n>
  //   <serial>  offline
  //   <serial>  unauthorized
  const targetLabel = (process.env.ANDROID_TARGET_MODEL ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();

  const deviceLines = stdout
    .split(/\r?\n/)
    .slice(1) // skip "List of devices attached" header
    .filter(
      (line) => /\bdevice\b/.test(line) && !/offline|unauthorized/.test(line),
    );

  if (deviceLines.length === 0) {
    return { available: false, serial: null, model: null };
  }

  // Prefer the line whose model/product matches ANDROID_TARGET_MODEL, else
  // fall back to the first authorized device.
  const preferred =
    (targetLabel
      ? deviceLines.find((line) =>
          line
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, " ")
            .includes(targetLabel),
        )
      : null) ?? deviceLines[0];

  const serial = preferred.split(/\s+/)[0] ?? null;

  const modelMatch = preferred.match(/\bmodel:(\S+)/);
  const model = modelMatch ? modelMatch[1].replace(/_/g, " ") : null;

  return { available: true, serial, model };
}

export async function startServer() {
  const port = await reservePort();
  const dataRoot = await mkdtemp(path.join(tmpdir(), "pixel-registry-"));
  const baseUrl = `http://127.0.0.1:${port}`;
  let logs = "";

  const serverProcess = spawn("node", ["apps/server/dist/index.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(port),
      HOST: "127.0.0.1",
      ANDROID_CONTROL_DATA_ROOT: dataRoot,
    },
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });

  serverProcess.stdout.on("data", (chunk) => {
    logs += chunk.toString();
  });
  serverProcess.stderr.on("data", (chunk) => {
    logs += chunk.toString();
  });

  await waitForHealth(baseUrl, () => logs);

  return {
    baseUrl,
    async stop() {
      serverProcess.kill();
      await new Promise((resolve) => serverProcess.once("exit", resolve));
      await rm(dataRoot, { recursive: true, force: true });
    },
  };
}

export async function reservePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error("Failed to reserve a TCP port"));
        return;
      }

      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(address.port);
      });
    });
  });
}

export async function waitForHealth(baseUrl, readLogs) {
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

    await new Promise((resolve) => setTimeout(resolve, 200));
  }

  throw new Error(`Server did not become healthy in time.\n${readLogs()}`);
}

export async function getJson(baseUrl, targetPath) {
  const response = await fetch(`${baseUrl}${targetPath}`);
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(payload?.error ?? JSON.stringify(payload));
  }
  return payload;
}

export async function postJson(baseUrl, targetPath, body) {
  const response = await fetch(`${baseUrl}${targetPath}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(payload?.error ?? JSON.stringify(payload));
  }
  return payload;
}
