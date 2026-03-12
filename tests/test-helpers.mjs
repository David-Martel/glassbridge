import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";

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
