import "./styles.css";
import { type RuntimeConfig, resolveRuntimeConfig } from "./runtime";

type ToolStatus = {
  vendorRoot: string;
  adb: { installed: boolean; path: string | null; version: string | null };
  scrcpy: { installed: boolean; path: string | null; version: string | null };
  registry?: { approvedCount: number; knownCount: number };
};

type DeviceRecord = {
  serial: string;
  state: string;
  model: string | null;
  product: string | null;
  device: string | null;
  transportId: string | null;
  alias?: string | null;
  approvalStatus?: string;
  approved?: boolean;
};

type HelpTopic = {
  id: string;
  title: string;
  bullets: string[];
};

type ApprovedDevice = {
  serial: string;
  alias: string | null;
  model: string | null;
  lastSeenAt: string;
};

type ScreenOrientation = "unknown" | "portrait" | "landscape" | "square";

const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {
  throw new Error("Missing #app root");
}

const state = {
  tools: null as ToolStatus | null,
  devices: [] as DeviceRecord[],
  approvedDevices: [] as ApprovedDevice[],
  selectedSerial: "",
  packages: [] as string[],
  helpTopics: [] as HelpTopic[],
  screenCaptureUrl: "",
  screenCaptureLoading: false,
  screenAutoRefresh: false,
  screenWidth: 0,
  screenHeight: 0,
  screenOrientation: "unknown" as ScreenOrientation,
  runtimeReady: false,
  runtimeLabel: "Launching desktop bridge",
  runtimeLaunchTarget: "",
  runtimeError: "",
  log: [] as string[],
};

let runtimeConfig: RuntimeConfig | null = null;
let activeSocket: WebSocket | null = null;
let screenRefreshTimer: number | null = null;

const quickControls = [
  { action: "home", label: "Home" },
  { action: "back", label: "Back" },
  { action: "recents", label: "Recents" },
  { action: "wake", label: "Wake" },
  { action: "power", label: "Power" },
  { action: "volumeUp", label: "Vol +" },
  { action: "volumeDown", label: "Vol -" },
];

render();
void bootstrapRuntime();

async function bootstrapRuntime() {
  activeSocket?.close();
  activeSocket = null;
  state.runtimeError = "";
  state.runtimeReady = false;
  state.runtimeLabel = "Launching desktop bridge";
  render();

  try {
    runtimeConfig = await resolveRuntimeConfig();
    state.runtimeReady = true;
    state.runtimeLabel = runtimeConfig.label;
    state.runtimeLaunchTarget = runtimeConfig.launchTarget;
    render();
    await initialize();
    connectSocket();
  } catch (error) {
    state.runtimeError =
      error instanceof Error ? error.message : "Runtime startup failed.";
    state.runtimeLabel = "Desktop launch failed";
    render();
  }
}

async function initialize() {
  await Promise.all([
    refreshTools(),
    refreshDevices(),
    refreshHelp(),
    refreshApprovedDevices(),
  ]);
}

async function refreshTools() {
  const response = await fetch(apiUrl("/api/tools/status"));
  const payload = await response.json();
  state.tools = payload.status;
  render();
}

async function refreshDevices() {
  const response = await fetch(apiUrl("/api/devices"));
  const payload = await response.json();
  state.devices = payload.devices;

  if (!state.selectedSerial && state.devices.length > 0) {
    state.selectedSerial = state.devices[0].serial;
  }

  if (state.selectedSerial) {
    await refreshPackages();
    await refreshScreenCapture();
  } else {
    state.packages = [];
    clearScreenCapture();
  }

  render();
}

async function refreshPackages() {
  if (!state.selectedSerial) {
    state.packages = [];
    return;
  }

  const response = await fetch(
    apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/apps`),
  );
  const payload = await response.json();
  state.packages = payload.packages;
  render();
}

async function refreshHelp() {
  const response = await fetch(apiUrl("/api/help/topics"));
  const payload = await response.json();
  state.helpTopics = payload.topics;
  render();
}

async function refreshApprovedDevices() {
  const response = await fetch(apiUrl("/api/approved-devices"));
  const payload = await response.json();
  state.approvedDevices = payload.devices;
  render();
}

async function refreshScreenCapture() {
  if (!state.selectedSerial) {
    clearScreenCapture();
    render();
    return;
  }

  state.screenCaptureLoading = true;
  render();

  try {
    const response = await fetch(
      apiUrl(
        `/api/devices/${encodeURIComponent(state.selectedSerial)}/screenshot?ts=${Date.now()}`,
      ),
      {
        cache: "no-store",
      },
    );
    if (!response.ok) {
      throw new Error(`Screen capture failed (${response.status})`);
    }

    const blob = await response.blob();
    replaceScreenCaptureUrl(URL.createObjectURL(blob));
  } catch (error) {
    pushLog(
      error instanceof Error
        ? error.message
        : "Screen capture failed unexpectedly.",
    );
  } finally {
    state.screenCaptureLoading = false;
    render();
  }
}

function connectSocket() {
  if (!runtimeConfig) {
    return;
  }

  activeSocket?.close();
  const socket = new WebSocket(runtimeConfig.wsUrl);
  activeSocket = socket;
  socket.onmessage = (event) => {
    const payload = JSON.parse(event.data);
    if (payload.type === "snapshot") {
      state.tools = payload.tools;
      state.devices = payload.devices;
      state.approvedDevices = payload.approvedDevices ?? state.approvedDevices;

      if (!state.selectedSerial && state.devices.length > 0) {
        state.selectedSerial = state.devices[0].serial;
      }

      render();
    }
    if (payload.type === "error") {
      pushLog(`Realtime update error: ${payload.message}`);
    }
  };
  socket.onclose = () => {
    if (activeSocket === socket) {
      setTimeout(connectSocket, 2000);
    }
  };
}

function render() {
  const selected =
    state.devices.find((device) => device.serial === state.selectedSerial) ??
    null;
  app.innerHTML = `
    <div class="shell">
      ${
        state.runtimeError
          ? `
            <section class="card alert-card">
              <div class="section-head">
                <h2>Desktop Launch Error</h2>
                <p>Bridge unavailable</p>
              </div>
              <p>${escapeHtml(state.runtimeError)}</p>
              <button data-action="retry-runtime">Retry Launch</button>
            </section>
          `
          : ""
      }
      <header class="hero card">
        <div>
          <p class="eyebrow">Android Ops Console</p>
          <h1>High-speed device control built on repo-local adb and scrcpy.</h1>
          <p class="lede">Use the roster, quick controls, app panel, and help rail to keep a phone session operable in real time.${state.runtimeReady && state.runtimeLaunchTarget ? ` Desktop bridge endpoint: ${escapeHtml(state.runtimeLaunchTarget)}.` : ""}</p>
        </div>
        <div class="hero-actions">
          ${renderChip("runtime", state.runtimeReady, state.runtimeLabel)}
          ${renderChip("adb", state.tools?.adb.installed ?? false, state.tools?.adb.version ?? "missing")}
          ${renderChip("scrcpy", state.tools?.scrcpy.installed ?? false, state.tools?.scrcpy.version ?? "missing")}
          ${renderChip("approved fleet", true, `${state.approvedDevices.length} saved`)}
          <button data-action="bootstrap" ${state.runtimeReady ? "" : "disabled"}>Bootstrap Tools</button>
          <button data-action="refresh" ${state.runtimeReady ? "" : "disabled"}>Refresh</button>
        </div>
      </header>

      <main class="layout">
        <section class="column">
          <section class="card">
            <div class="section-head">
              <h2>Device Roster</h2>
              <p>${state.devices.length} connected</p>
            </div>
            <div class="device-list">
              ${state.devices.length ? state.devices.map(renderDevice).join("") : `<p class="empty">No device detected. Check USB debugging, cable quality, and RSA authorization.</p>`}
            </div>
          </section>

          <section class="card">
            <div class="section-head">
              <h2>Approved Fleet</h2>
              <p>${state.approvedDevices.length} saved</p>
            </div>
            <div class="package-list">
              ${state.approvedDevices.length ? state.approvedDevices.map(renderApprovedDevice).join("") : `<p class="empty">No approved devices saved yet.</p>`}
            </div>
          </section>

          <section class="card">
            <div class="section-head">
              <h2>Quick Interaction</h2>
              <p>${selected ? selected.serial : "Select a device"}</p>
            </div>
            <div class="button-grid">
              ${quickControls.map((item) => `<button data-control="${item.action}" ${selected ? "" : "disabled"}>${item.label}</button>`).join("")}
            </div>
            <form class="inline-form" data-form="tap">
              <label>X<input name="x" type="number" value="400" /></label>
              <label>Y<input name="y" type="number" value="900" /></label>
              <button ${selected ? "" : "disabled"}>Tap</button>
            </form>
            <form class="inline-form" data-form="text">
              <label class="wide">Text<input name="text" type="text" placeholder="Type on the phone" /></label>
              <button ${selected ? "" : "disabled"}>Send Text</button>
            </form>
          </section>

          <section class="card">
            <div class="section-head">
              <h2>Live Control</h2>
              <p>scrcpy launch presets</p>
            </div>
            <div class="preset-list">
              <button data-scrcpy="latency" ${selected ? "" : "disabled"}>Low Latency</button>
              <button data-scrcpy="balanced" ${selected ? "" : "disabled"}>Balanced</button>
              <button data-scrcpy="quality" ${selected ? "" : "disabled"}>High Quality</button>
            </div>
            <p class="hint">Use scrcpy for low-latency mirroring and direct interaction. The API UI is for orchestration and fast device ops.</p>
          </section>
        </section>

        <section class="column">
          <section class="card">
            <div class="section-head">
              <h2>Screen Preview</h2>
              <p>${selected ? renderScreenSummary() : "Select a device"}</p>
            </div>
            <div class="toolbar screen-toolbar">
              <button data-action="refresh-screen" ${selected ? "" : "disabled"}>
                ${state.screenCaptureLoading ? "Refreshing..." : "Refresh Screen"}
              </button>
              <label class="toggle">
                <input type="checkbox" data-action="screen-auto-refresh" ${state.screenAutoRefresh ? "checked" : ""} ${selected ? "" : "disabled"} />
                <span>Auto refresh</span>
              </label>
            </div>
            ${
              state.screenCaptureUrl
                ? `
                  <div class="screen-panel screen-panel-${state.screenOrientation}">
                    <div class="screen-frame screen-frame-${state.screenOrientation}">
                      <img
                        src="${state.screenCaptureUrl}"
                        alt="Device screen preview"
                        class="screen-preview screen-preview-${state.screenOrientation}"
                        data-screen-preview
                        draggable="false"
                      />
                    </div>
                    <div class="screen-inspector">
                      <div class="screen-stat">
                        <span>Orientation</span>
                        <strong>${formatScreenOrientation(state.screenOrientation)}</strong>
                      </div>
                      <div class="screen-stat">
                        <span>Preview size</span>
                        <strong>${state.screenWidth > 0 && state.screenHeight > 0 ? `${state.screenWidth} x ${state.screenHeight}` : "Waiting for frame"}</strong>
                      </div>
                      <div class="screen-stat">
                        <span>Layout mode</span>
                        <strong>${state.screenOrientation === "landscape" ? "Wide dock" : "Vertical dock"}</strong>
                      </div>
                      <p class="hint">${renderScreenHint()}</p>
                    </div>
                  </div>
                `
                : `<p class="empty">No screen capture loaded yet.</p>`
            }
          </section>

          <section class="card">
            <div class="section-head">
              <h2>App Workspace</h2>
              <p>${selected ? "Third-party packages" : "Select a device"}</p>
            </div>
            <div class="toolbar">
              <button data-action="packages" ${selected ? "" : "disabled"}>Refresh Packages</button>
            </div>
            <div class="package-list">
              ${state.packages.length ? state.packages.map(renderPackage).join("") : `<p class="empty">No package list loaded yet.</p>`}
            </div>
          </section>

          <section class="card">
            <div class="section-head">
              <h2>Command Console</h2>
              <p>Raw adb shell</p>
            </div>
            <form class="stack-form" data-form="shell">
              <input name="command" type="text" placeholder="getprop ro.product.model" ${selected ? "" : "disabled"} />
              <button ${selected ? "" : "disabled"}>Run</button>
            </form>
          </section>

          <section class="card">
            <div class="section-head">
              <h2>Event Log</h2>
              <p>Latest responses</p>
            </div>
            <div class="log-list">
              ${state.log.length ? state.log.map((entry) => `<pre>${escapeHtml(entry)}</pre>`).join("") : `<p class="empty">No events yet.</p>`}
            </div>
          </section>
        </section>

        <aside class="card help-rail">
          <div class="section-head">
            <h2>Guided Help</h2>
            <p>Operator recovery</p>
          </div>
          ${state.helpTopics
            .map(
              (topic) => `
            <section class="help-topic">
              <h3>${topic.title}</h3>
              <ul>
                ${topic.bullets.map((bullet) => `<li>${bullet}</li>`).join("")}
              </ul>
            </section>
          `,
            )
            .join("")}
        </aside>
      </main>
    </div>
  `;

  bindEvents();
}

function bindEvents() {
  bindAction("retry-runtime", async () => {
    await bootstrapRuntime();
  });

  bindAction("bootstrap", async () => {
    pushLog("Bootstrapping local Android tools...");
    await post(apiUrl("/api/tools/bootstrap"));
    await refreshTools();
    await refreshApprovedDevices();
    pushLog("Tool bootstrap complete.");
  });

  bindAction("refresh", async () => {
    await initialize();
    pushLog("Refreshed tools, devices, and help.");
  });

  bindAction("packages", async () => {
    await refreshPackages();
    pushLog(`Refreshed packages for ${state.selectedSerial}`);
  });

  bindAction("refresh-screen", async () => {
    await refreshScreenCapture();
    pushLog(`Refreshed screen preview for ${state.selectedSerial}`);
  });

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-select]",
  )) {
    button.onclick = async () => {
      state.selectedSerial = button.dataset.select ?? "";
      await refreshPackages();
      await refreshScreenCapture();
      render();
    };
  }

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-approve]",
  )) {
    button.onclick = async (event) => {
      event.stopPropagation();
      const serial = button.dataset.approve ?? "";
      const device = state.devices.find((entry) => entry.serial === serial);
      await post(
        apiUrl(`/api/approved-devices/${encodeURIComponent(serial)}/approve`),
        {
          alias: device?.alias ?? device?.model ?? undefined,
        },
      );
      await refreshApprovedDevices();
      await refreshDevices();
      pushLog(`Approved ${serial} for future reuse.`);
    };
  }

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-control]",
  )) {
    button.onclick = async () => {
      const action = button.dataset.control ?? "";
      await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/control`,
        ),
        { action },
      );
      pushLog(`Sent ${action} to ${state.selectedSerial}`);
    };
  }

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-scrcpy]",
  )) {
    button.onclick = async () => {
      const preset = button.dataset.scrcpy ?? "balanced";
      await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/scrcpy`,
        ),
        { preset },
      );
      pushLog(`Launched scrcpy (${preset}) for ${state.selectedSerial}`);
    };
  }

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-launch]",
  )) {
    button.onclick = async () => {
      const packageName = button.dataset.launch ?? "";
      await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/apps/start`,
        ),
        { packageName },
      );
      pushLog(`Launched ${packageName}`);
    };
  }

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-stop]",
  )) {
    button.onclick = async () => {
      const packageName = button.dataset.stop ?? "";
      await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/apps/stop`,
        ),
        { packageName },
      );
      pushLog(`Force stopped ${packageName}`);
    };
  }

  const tapForm = document.querySelector<HTMLFormElement>("[data-form='tap']");
  if (tapForm) {
    tapForm.onsubmit = async (event) => {
      event.preventDefault();
      const formData = new FormData(tapForm);
      const x = Number(formData.get("x"));
      const y = Number(formData.get("y"));
      await post(
        apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/tap`),
        { x, y },
      );
      pushLog(`Tapped ${x},${y}`);
    };
  }

  const textForm =
    document.querySelector<HTMLFormElement>("[data-form='text']");
  if (textForm) {
    textForm.onsubmit = async (event) => {
      event.preventDefault();
      const formData = new FormData(textForm);
      const text = String(formData.get("text") ?? "");
      await post(
        apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/text`),
        { text },
      );
      pushLog(`Sent text to ${state.selectedSerial}`);
      textForm.reset();
    };
  }

  const shellForm = document.querySelector<HTMLFormElement>(
    "[data-form='shell']",
  );
  if (shellForm) {
    shellForm.onsubmit = async (event) => {
      event.preventDefault();
      const formData = new FormData(shellForm);
      const command = String(formData.get("command") ?? "");
      const result = await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/shell`,
        ),
        { command },
      );
      pushLog(
        `$ ${command}\n${result.stdout || result.stderr || "[no output]"}`,
      );
    };
  }

  const screenToggle = document.querySelector<HTMLInputElement>(
    "[data-action='screen-auto-refresh']",
  );
  if (screenToggle) {
    screenToggle.onchange = async () => {
      state.screenAutoRefresh = screenToggle.checked;
      syncScreenRefreshTimer();
      if (state.screenAutoRefresh) {
        await refreshScreenCapture();
      } else {
        render();
      }
    };
  }

  const screenPreview = document.querySelector<HTMLImageElement>(
    "[data-screen-preview]",
  );
  if (screenPreview) {
    const syncMetrics = () => {
      updateScreenMetrics(
        screenPreview.naturalWidth,
        screenPreview.naturalHeight,
      );
    };
    screenPreview.onload = syncMetrics;
    if (screenPreview.complete) {
      syncMetrics();
    }

    screenPreview.onclick = async (event) => {
      if (!state.selectedSerial) {
        return;
      }

      if (!screenPreview.naturalWidth || !screenPreview.naturalHeight) {
        return;
      }

      const rect = screenPreview.getBoundingClientRect();
      const scaleX = screenPreview.naturalWidth / rect.width;
      const scaleY = screenPreview.naturalHeight / rect.height;
      const rawX = Math.round((event.clientX - rect.left) * scaleX);
      const rawY = Math.round((event.clientY - rect.top) * scaleY);
      const x = Math.max(0, Math.min(screenPreview.naturalWidth - 1, rawX));
      const y = Math.max(0, Math.min(screenPreview.naturalHeight - 1, rawY));

      await post(
        apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/tap`),
        { x, y },
      );
      pushLog(`Tapped screen preview at ${x},${y}`);
      if (!state.screenAutoRefresh) {
        await refreshScreenCapture();
      }
    };
  }

  syncScreenRefreshTimer();
}

function bindAction(action: string, handler: () => Promise<void>) {
  const button = document.querySelector<HTMLButtonElement>(
    `[data-action='${action}']`,
  );
  if (!button) {
    return;
  }
  button.onclick = async () => {
    await handler();
  };
}

function apiUrl(path: string) {
  if (!runtimeConfig) {
    throw new Error("Runtime config is not ready yet.");
  }

  return `${runtimeConfig.apiRoot}${path}`;
}

async function post(url: string, body?: unknown) {
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body ?? {}),
  });
  const payload = await response.json();
  if (!response.ok) {
    const message = payload?.error ?? `Request failed (${response.status})`;
    pushLog(message);
    throw new Error(message);
  }
  return payload;
}

function pushLog(entry: string) {
  state.log = [
    `[${new Date().toLocaleTimeString()}] ${entry}`,
    ...state.log,
  ].slice(0, 14);
  render();
}

function syncScreenRefreshTimer() {
  if (screenRefreshTimer !== null) {
    window.clearInterval(screenRefreshTimer);
    screenRefreshTimer = null;
  }

  if (state.screenAutoRefresh && state.selectedSerial) {
    screenRefreshTimer = window.setInterval(() => {
      void refreshScreenCapture();
    }, 2500);
  }
}

function clearScreenCapture() {
  replaceScreenCaptureUrl("");
  state.screenCaptureLoading = false;
  updateScreenMetrics(0, 0);
}

function replaceScreenCaptureUrl(nextUrl: string) {
  if (state.screenCaptureUrl.startsWith("blob:")) {
    URL.revokeObjectURL(state.screenCaptureUrl);
  }
  state.screenCaptureUrl = nextUrl;
}

function updateScreenMetrics(width: number, height: number) {
  const orientation = getScreenOrientation(width, height);
  if (
    state.screenWidth === width &&
    state.screenHeight === height &&
    state.screenOrientation === orientation
  ) {
    return;
  }

  state.screenWidth = width;
  state.screenHeight = height;
  state.screenOrientation = orientation;
  render();
}

function getScreenOrientation(
  width: number,
  height: number,
): ScreenOrientation {
  if (!width || !height) {
    return "unknown";
  }
  if (width === height) {
    return "square";
  }
  return width > height ? "landscape" : "portrait";
}

function renderScreenSummary() {
  if (!state.screenWidth || !state.screenHeight) {
    return "Click preview to tap the phone";
  }

  return `${formatScreenOrientation(state.screenOrientation)} • ${state.screenWidth} x ${state.screenHeight}`;
}

function renderScreenHint() {
  if (state.screenOrientation === "landscape") {
    return "Landscape previews widen automatically so the phone surface stays unobstructed and diagnostics shift to the side.";
  }

  if (state.screenOrientation === "portrait") {
    return "Portrait previews keep the device centered with guidance stacked underneath for one-handed tapping and reading.";
  }

  return "Tap directly on the preview to send scaled input. Use scrcpy for continuous low-latency interaction.";
}

function formatScreenOrientation(orientation: ScreenOrientation) {
  if (orientation === "landscape") {
    return "Landscape";
  }
  if (orientation === "portrait") {
    return "Portrait";
  }
  if (orientation === "square") {
    return "Square";
  }
  return "Unknown";
}

function renderChip(label: string, ready: boolean, version: string) {
  return `<div class="chip ${ready ? "ready" : "missing"}"><span>${label}</span><strong>${version}</strong></div>`;
}

function renderDevice(device: DeviceRecord) {
  return `
    <button class="device-card ${device.serial === state.selectedSerial ? "selected" : ""}" data-select="${device.serial}">
      <div class="device-top">
        <strong>${device.alias ?? device.model ?? device.serial}</strong>
        <span class="badge badge-${device.state}">${device.state}</span>
      </div>
      <p>${device.serial}</p>
      <p>${device.product ?? "unknown product"} / ${device.device ?? "unknown device"}</p>
      <p>transport ${device.transportId ?? "n/a"}</p>
      <div class="device-footer">
        <span class="badge ${device.approved ? "badge-approved" : "badge-candidate"}">${device.approved ? "approved" : "candidate"}</span>
        <button type="button" data-approve="${device.serial}" ${device.approved ? "disabled" : ""}>${device.approved ? "Saved" : "Approve"}</button>
      </div>
    </button>
  `;
}

function renderApprovedDevice(device: ApprovedDevice) {
  return `
    <div class="package-row">
      <div>
        <strong>${device.alias ?? device.model ?? device.serial}</strong>
        <p>${device.serial}</p>
      </div>
      <span class="fleet-time">${new Date(device.lastSeenAt).toLocaleString()}</span>
    </div>
  `;
}

function renderPackage(packageName: string) {
  return `
    <div class="package-row">
      <code>${packageName}</code>
      <div class="row-actions">
        <button data-launch="${packageName}" ${state.selectedSerial ? "" : "disabled"}>Launch</button>
        <button data-stop="${packageName}" ${state.selectedSerial ? "" : "disabled"}>Force Stop</button>
      </div>
    </div>
  `;
}

function escapeHtml(value: string) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}
