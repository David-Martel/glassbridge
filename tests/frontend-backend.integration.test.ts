import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { getJson, startServer } from "./test-helpers.mjs";

class MockWebSocket {
  onmessage = null;
  onclose = null;
}

let server: Awaited<ReturnType<typeof startServer>>;

beforeEach(async () => {
  server = await startServer();
  document.body.innerHTML = `<div id="app"></div>`;
  const tools = await getJson(server.baseUrl, "/api/tools/status");
  const devices = await getJson(server.baseUrl, "/api/devices");
  const help = await getJson(server.baseUrl, "/api/help/topics");
  const approved = await getJson(server.baseUrl, "/api/approved-devices");
  const selectedSerial = devices.devices[0]?.serial;
  const packages = selectedSerial
    ? await getJson(
        server.baseUrl,
        `/api/devices/${encodeURIComponent(selectedSerial)}/apps`,
      )
    : { packages: [] };

  const livePayloads = new Map<string, unknown>([
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

    const normalizedPath = input.startsWith(server.baseUrl)
      ? input.slice(server.baseUrl.length)
      : input;
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
});

afterEach(async () => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
  await server.stop();
});

test("frontend renders against live backend payloads from the connected device", async () => {
  const devices = await getJson(server.baseUrl, "/api/devices");
  expect(devices.devices.length).toBeGreaterThan(0);

  vi.resetModules();
  await import("../apps/web/src/main.ts");
  await settle();

  expect(document.body.textContent).toContain(devices.devices[0].serial);
  expect(document.body.textContent).toContain("Approved Fleet");
  expect(document.body.textContent).toContain("Guided Help");
});

async function settle() {
  for (let index = 0; index < 8; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}
