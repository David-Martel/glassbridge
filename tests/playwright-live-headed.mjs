import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { chromium } from "playwright";
import { reservePort, waitForHealth } from "./test-helpers.mjs";

const outputDir = path.join(process.cwd(), "output", "playwright");
mkdirSync(outputDir, { recursive: true });

const serverPort = await reservePort();
const dataRoot = await mkdtemp(path.join(tmpdir(), "pixel-playwright-"));
const baseUrl = `http://127.0.0.1:${serverPort}`;
const browserExecutable = findBrowserExecutable();

assert.ok(
  browserExecutable,
  "No Chrome or Edge executable was found for headed Playwright.",
);

const serverProcess = spawn("node", ["apps/server/dist/index.js"], {
  cwd: process.cwd(),
  env: {
    ...process.env,
    HOST: "127.0.0.1",
    PORT: String(serverPort),
    ANDROID_CONTROL_DATA_ROOT: dataRoot,
  },
  stdio: ["ignore", "pipe", "pipe"],
  windowsHide: true,
});

let serverLogs = "";
serverProcess.stdout.on("data", (chunk) => {
  serverLogs += chunk.toString();
});
serverProcess.stderr.on("data", (chunk) => {
  serverLogs += chunk.toString();
});

let browser;

try {
  await waitForHealth(baseUrl, () => serverLogs);

  const devicesPayload = await getJson(`${baseUrl}/api/devices`);
  const activeDevices = devicesPayload.devices.filter(
    (device) => device.state === "device",
  );
  assert.ok(
    activeDevices.length > 0,
    "No authorized Android device is connected for the Playwright proof run.",
  );
  const device = activeDevices[0];

  browser = await chromium.launch({
    headless: false,
    executablePath: browserExecutable,
    slowMo: 150,
  });

  const page = await browser.newPage({
    viewport: { width: 1600, height: 1200 },
  });
  let screenshotResponseCount = 0;
  page.on("response", (response) => {
    if (response.url().includes("/screenshot")) {
      screenshotResponseCount += 1;
    }
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.screenshot({
    path: path.join(outputDir, "live-ui-initial.png"),
    fullPage: true,
  });

  const deviceCard = page
    .getByRole("button", { name: new RegExp(device.serial) })
    .first();
  await deviceCard.waitFor({ timeout: 15000 });
  const screenPreview = page.getByAltText("Device screen preview");
  await screenPreview.waitFor({ timeout: 15000 });
  await waitForScreenshotResponses(() => screenshotResponseCount, 1);
  await assertImageDecoded(screenPreview);

  const initialResponseCount = screenshotResponseCount;
  await page.getByRole("button", { name: /Refresh Screen/i }).click();
  await waitForScreenshotResponses(
    () => screenshotResponseCount,
    initialResponseCount + 1,
  );
  await assertImageDecoded(screenPreview);

  await page.getByRole("button", { name: /Wake/i }).click();
  await screenPreview.click({
    position: { x: 120, y: 240 },
  });
  await page.getByText(/Tapped screen preview at \d+,\d+/).waitFor({
    timeout: 15000,
  });

  const shellInput = page.getByPlaceholder("getprop ro.product.model");
  await shellInput.fill("getprop ro.product.model");
  await page.getByRole("button", { name: /^Run$/ }).click();
  await page.getByText(new RegExp(device.model ?? "Pixel", "i")).waitFor({
    timeout: 15000,
  });

  const approveButton = page.locator(`[data-approve="${device.serial}"]`);
  const approveLabel = await approveButton.textContent();
  if (approveLabel?.includes("Approve")) {
    await approveButton.click();
    await page
      .locator(`[data-approve="${device.serial}"]`)
      .getByText(/Saved/i)
      .waitFor({
        timeout: 10000,
      });
  }

  const postManualResponseCount = screenshotResponseCount;
  await page.getByLabel(/Auto refresh/i).check();
  await waitForScreenshotResponses(
    () => screenshotResponseCount,
    postManualResponseCount + 1,
    12000,
  );
  await assertImageDecoded(screenPreview);

  await killScrcpyIfRunning();
  await page.getByRole("button", { name: /Low Latency/i }).click();
  await page
    .getByText(new RegExp(`Launched scrcpy.*${device.serial}`))
    .waitFor({
      timeout: 15000,
    });
  await waitForScrcpyProcess();

  await page.screenshot({
    path: path.join(outputDir, "live-ui-after-controls.png"),
  });

  console.log(
    `Headed Playwright validation passed against ${device.serial} (${device.model}).`,
  );
  console.log(`Artifacts saved under ${outputDir}`);
} finally {
  await killScrcpyIfRunning();
  await browser?.close().catch(() => undefined);
  serverProcess.kill();
  await new Promise((resolve) => serverProcess.once("exit", resolve));
  await rm(dataRoot, { recursive: true, force: true });
}

async function getJson(url) {
  const response = await fetch(url);
  const payload = await response.json();
  assert.equal(response.ok, true, payload?.error ?? JSON.stringify(payload));
  return payload;
}

function findBrowserExecutable() {
  const candidates = [
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  ];

  return candidates.find((candidate) => existsSync(candidate)) ?? null;
}

async function assertImageDecoded(locator) {
  const decoded = await locator.evaluate((element) => {
    if (!(element instanceof HTMLImageElement)) {
      return { complete: false, naturalWidth: 0, naturalHeight: 0 };
    }

    return {
      complete: element.complete,
      naturalWidth: element.naturalWidth,
      naturalHeight: element.naturalHeight,
    };
  });

  assert.equal(
    decoded.complete,
    true,
    "Screen preview image did not finish loading.",
  );
  assert.ok(decoded.naturalWidth > 0, "Screen preview image width was zero.");
  assert.ok(decoded.naturalHeight > 0, "Screen preview image height was zero.");
}

async function waitForScreenshotResponses(
  readCount,
  targetCount,
  timeoutMs = 15000,
) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (readCount() >= targetCount) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }

  throw new Error(
    `Expected at least ${targetCount} screenshot responses, received ${readCount()}.`,
  );
}

async function waitForScrcpyProcess() {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (isScrcpyRunning()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
  }

  throw new Error("scrcpy did not start after the UI launch action.");
}

function isScrcpyRunning() {
  const output = execFileSync("tasklist", ["/FI", "IMAGENAME eq scrcpy.exe"], {
    encoding: "utf8",
  });
  return output.toLowerCase().includes("scrcpy.exe");
}

async function killScrcpyIfRunning() {
  try {
    execFileSync("taskkill", ["/IM", "scrcpy.exe", "/F"], {
      stdio: "ignore",
    });
  } catch {
    // No scrcpy process was running.
  }
}
