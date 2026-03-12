import { access } from "node:fs/promises";
import cors from "@fastify/cors";
import fastifyStatic from "@fastify/static";
import websocket from "@fastify/websocket";
import Fastify from "fastify";
import { z } from "zod";
import {
  captureScreenshot,
  type DeviceRecord,
  inputText,
  launchPackage,
  listDevices,
  listPackages,
  sendKey,
  shellCommand,
  startScrcpy,
  stopPackage,
  tap,
} from "./adb.js";
import { host, port, webDistRoot } from "./config.js";
import { helpTopics } from "./help.js";
import {
  approveDevice,
  getRegistryMap,
  getRegistrySummary,
  listApprovedDevices,
  syncSeenDevices,
} from "./registry.js";
import { bootstrapTools, getToolStatus, readToolManifest } from "./tooling.js";

const app = Fastify({ logger: true });
type SnapshotSocket = {
  send: (payload: string) => void;
  on: (event: string, cb: () => void) => void;
};
const sockets = new Set<SnapshotSocket>();

await app.register(cors, { origin: true });
await app.register(websocket);

if (await pathExists(webDistRoot)) {
  await app.register(fastifyStatic, {
    root: webDistRoot,
  });
}

app.get("/api/health", async () => ({ ok: true }));

app.get("/api/tools/status", async () => ({
  status: await getToolStatus(),
  manifest: await readToolManifest(),
  registry: getRegistrySummary(),
}));

app.post("/api/tools/bootstrap", async () => ({
  output: await bootstrapTools(),
  status: await getToolStatus(),
}));

app.get("/api/devices", async () => ({
  devices: await snapshotDevices(),
}));

app.get("/api/devices/:serial/apps", async (request) => {
  const { serial } = request.params as { serial: string };
  return { packages: await listPackages(serial) };
});

app.get("/api/devices/:serial/screenshot", async (request, reply) => {
  const { serial } = request.params as { serial: string };
  const screenshot = await captureScreenshot(serial);
  reply
    .header("Content-Type", "image/png")
    .header("Cache-Control", "no-store")
    .send(screenshot);
});

app.get("/api/approved-devices", async () => ({
  devices: listApprovedDevices(),
}));

app.get("/api/help/topics", async () => ({
  topics: helpTopics,
}));

app.post("/api/approved-devices/:serial/approve", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z
    .object({
      alias: z.string().trim().min(1).optional(),
      notes: z.string().trim().min(1).optional(),
    })
    .parse(request.body ?? {});

  const devices = await listDevices();
  const target = devices.find((device) => device.serial === serial);
  if (!target) {
    throw new Error(`Connected device ${serial} was not found`);
  }

  syncSeenDevices(devices);
  approveDevice(target, {
    alias: body.alias,
    notes: body.notes,
    approvalSource: "local-ui",
  });

  return {
    ok: true,
    approvedDevice:
      listApprovedDevices().find((device) => device.serial === serial) ?? null,
  };
});

app.post("/api/devices/:serial/control", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ action: z.string() }).parse(request.body);
  return sendKey(serial, body.action);
});

app.post("/api/devices/:serial/tap", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ x: z.number(), y: z.number() }).parse(request.body);
  return tap(serial, body.x, body.y);
});

app.post("/api/devices/:serial/text", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ text: z.string() }).parse(request.body);
  return inputText(serial, body.text);
});

app.post("/api/devices/:serial/apps/start", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ packageName: z.string().min(1) }).parse(request.body);
  return launchPackage(serial, body.packageName);
});

app.post("/api/devices/:serial/apps/stop", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ packageName: z.string().min(1) }).parse(request.body);
  return stopPackage(serial, body.packageName);
});

app.post("/api/devices/:serial/shell", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z.object({ command: z.string().min(1) }).parse(request.body);
  return shellCommand(serial, body.command);
});

app.post("/api/devices/:serial/scrcpy", async (request) => {
  const { serial } = request.params as { serial: string };
  const body = z
    .object({ preset: z.enum(["latency", "balanced", "quality"]) })
    .parse(request.body);
  return startScrcpy(serial, body.preset);
});

app.get("/ws", { websocket: true }, (socket) => {
  sockets.add(socket);
  socket.on("close", () => {
    sockets.delete(socket);
  });
});

setInterval(async () => {
  if (!sockets.size) {
    return;
  }

  try {
    const payload = JSON.stringify({
      type: "snapshot",
      tools: await getToolStatus(),
      devices: await snapshotDevices(),
      approvedDevices: listApprovedDevices(),
      registry: getRegistrySummary(),
    });

    for (const socket of sockets) {
      socket.send(payload);
    }
  } catch (error) {
    const payload = JSON.stringify({
      type: "error",
      message:
        error instanceof Error ? error.message : "Unknown update failure",
    });

    for (const socket of sockets) {
      socket.send(payload);
    }
  }
}, 2000);

app.setErrorHandler((error, _request, reply) => {
  reply.status(500).send({
    error: error instanceof Error ? error.message : "Unknown server error",
  });
});

await app.listen({ host, port });
app.log.info(`Android control server listening on http://${host}:${port}`);

async function pathExists(targetPath: string) {
  try {
    await access(targetPath);
    return true;
  } catch {
    return false;
  }
}

async function snapshotDevices() {
  const devices = await listDevices();
  syncSeenDevices(devices);
  const registry = getRegistryMap();

  return devices.map((device) =>
    enrichDevice(device, registry.get(device.serial)),
  );
}

function enrichDevice(
  device: DeviceRecord,
  registryRecord?: {
    alias: string | null;
    approvalStatus: string;
    notes: string | null;
    firstSeenAt: string;
    lastSeenAt: string;
  },
) {
  return {
    ...device,
    alias: registryRecord?.alias ?? null,
    approvalStatus: registryRecord?.approvalStatus ?? "candidate",
    approved: registryRecord?.approvalStatus === "approved",
    notes: registryRecord?.notes ?? null,
    firstSeenAt: registryRecord?.firstSeenAt ?? null,
    lastSeenAt: registryRecord?.lastSeenAt ?? null,
  };
}
