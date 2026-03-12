import { afterEach, expect, test, vi } from "vitest";

const devices = [
  {
    serial: "MOCK-PIXEL10PROXL-001",
    state: "device",
    model: "Pixel 10 Pro XL",
    product: "mustang",
    device: "mustang",
    transportId: "101",
    approved: false,
    approvalStatus: "candidate",
    alias: null,
  },
];

const approvedDevices: Array<{
  serial: string;
  alias: string | null;
  model: string | null;
  lastSeenAt: string;
}> = [];

class MockWebSocket {
  onmessage = null;
  onclose = null;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

test("frontend renders roster and posts approval actions", async () => {
  document.body.innerHTML = `<div id="app"></div>`;

  const fetchMock = vi.fn(async (input: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";

    if (method === "POST" && input.includes("/api/approved-devices/")) {
      approvedDevices.push({
        serial: "MOCK-PIXEL10PROXL-001",
        alias: "Pixel 10 Pro XL",
        model: "Pixel 10 Pro XL",
        lastSeenAt: new Date().toISOString(),
      });
      devices[0].approved = true;
      devices[0].approvalStatus = "approved";
      devices[0].alias = "Pixel 10 Pro XL";
      return jsonResponse({ ok: true });
    }

    if (input.endsWith("/api/tools/status")) {
      return jsonResponse({
        status: {
          vendorRoot: "mock://vendor",
          adb: { installed: true, path: "mock:adb", version: "adb mock" },
          scrcpy: {
            installed: true,
            path: "mock:scrcpy",
            version: "scrcpy mock",
          },
          registry: { approvedCount: approvedDevices.length, knownCount: 1 },
        },
      });
    }

    if (input.endsWith("/api/devices")) {
      return jsonResponse({ devices });
    }

    if (input.endsWith("/api/help/topics")) {
      return jsonResponse({
        topics: [
          { id: "connect", title: "Connect", bullets: ["Allow USB debugging"] },
        ],
      });
    }

    if (input.endsWith("/api/approved-devices")) {
      return jsonResponse({ devices: approvedDevices });
    }

    if (input.includes("/api/devices/") && input.endsWith("/apps")) {
      return jsonResponse({ packages: ["com.example.pixelapp"] });
    }

    return jsonResponse({ ok: true });
  });

  vi.stubGlobal("fetch", fetchMock);
  vi.stubGlobal("WebSocket", MockWebSocket);

  vi.resetModules();
  await import("../apps/web/src/main.ts");
  await settle();

  expect(document.body.textContent).toContain("Pixel 10 Pro XL");
  expect(document.body.textContent).toContain("Guided Help");

  const approveButton = [...document.querySelectorAll("button")].find(
    (button) => button.textContent?.includes("Approve"),
  );
  expect(approveButton).toBeTruthy();

  approveButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  await settle();

  expect(fetchMock).toHaveBeenCalledWith(
    expect.stringContaining(
      "/api/approved-devices/MOCK-PIXEL10PROXL-001/approve",
    ),
    expect.objectContaining({ method: "POST" }),
  );
  expect(document.body.textContent).toContain("Approved Fleet");
});

function jsonResponse(payload: unknown) {
  return Promise.resolve(
    new Response(JSON.stringify(payload), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    }),
  );
}

async function settle() {
  for (let index = 0; index < 6; index += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}
