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
const screenshotBytes = Uint8Array.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49,
  0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04,
  0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44,
  0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xff, 0x1f, 0x00, 0x03, 0x03, 0x02, 0x00,
  0xef, 0x4f, 0xc5, 0xd3, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
  0x42, 0x60, 0x82,
]);

class MockWebSocket {
  onmessage = null;
  onclose = null;
}

const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalCreateObjectURL) {
    URL.createObjectURL = originalCreateObjectURL;
  } else {
    delete URL.createObjectURL;
  }
  if (originalRevokeObjectURL) {
    URL.revokeObjectURL = originalRevokeObjectURL;
  } else {
    delete URL.revokeObjectURL;
  }
  document.body.innerHTML = "";
});

test("frontend renders roster and posts approval actions", async () => {
  document.body.innerHTML = `<div id="app"></div>`;
  URL.createObjectURL = vi.fn(() => "blob:mock-screen-preview");
  URL.revokeObjectURL = vi.fn();

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

    if (input.includes("/api/devices/") && input.includes("/screenshot")) {
      return new Response(screenshotBytes, {
        status: 200,
        headers: { "Content-Type": "image/png" },
      });
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
  const screenPreview = await waitForScreenPreview();
  expect(screenPreview).toBeTruthy();
  expect(screenPreview?.src ?? "").toContain("blob:mock-screen-preview");

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

async function waitForScreenPreview(timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const screenPreview = document.querySelector<HTMLImageElement>(
      "img[alt='Device screen preview']",
    );
    if (screenPreview) {
      return screenPreview;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }

  return null;
}
