import "./styles.css";

declare global {
  interface Window {
    __PIXEL_API_ORIGIN__?: string;
    __PIXEL_WS_URL__?: string;
  }
}

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

const apiRoot = window.__PIXEL_API_ORIGIN__ ?? "";
const wsUrl =
  window.__PIXEL_WS_URL__ ??
  `${window.location.protocol === "https:" ? "wss" : "ws"}://${window.location.host}/ws`;
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
  log: [] as string[],
};

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
void initialize();
connectSocket();

async function initialize() {
  await Promise.all([
    refreshTools(),
    refreshDevices(),
    refreshHelp(),
    refreshApprovedDevices(),
  ]);
}

async function refreshTools() {
  const response = await fetch(`${apiRoot}/api/tools/status`);
  const payload = await response.json();
  state.tools = payload.status;
  render();
}

async function refreshDevices() {
  const response = await fetch(`${apiRoot}/api/devices`);
  const payload = await response.json();
  state.devices = payload.devices;

  if (!state.selectedSerial && state.devices.length > 0) {
    state.selectedSerial = state.devices[0].serial;
  }

  if (state.selectedSerial) {
    await refreshPackages();
  } else {
    state.packages = [];
  }

  render();
}

async function refreshPackages() {
  if (!state.selectedSerial) {
    state.packages = [];
    return;
  }

  const response = await fetch(
    `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/apps`,
  );
  const payload = await response.json();
  state.packages = payload.packages;
  render();
}

async function refreshHelp() {
  const response = await fetch(`${apiRoot}/api/help/topics`);
  const payload = await response.json();
  state.helpTopics = payload.topics;
  render();
}

async function refreshApprovedDevices() {
  const response = await fetch(`${apiRoot}/api/approved-devices`);
  const payload = await response.json();
  state.approvedDevices = payload.devices;
  render();
}

function connectSocket() {
  const socket = new WebSocket(wsUrl);
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
    setTimeout(connectSocket, 2000);
  };
}

function render() {
  const selected =
    state.devices.find((device) => device.serial === state.selectedSerial) ??
    null;
  app.innerHTML = `
    <div class="shell">
      <header class="hero card">
        <div>
          <p class="eyebrow">Android Ops Console</p>
          <h1>High-speed device control built on repo-local adb and scrcpy.</h1>
          <p class="lede">Use the roster, quick controls, app panel, and help rail to keep a phone session operable in real time.</p>
        </div>
        <div class="hero-actions">
          ${renderChip("adb", state.tools?.adb.installed ?? false, state.tools?.adb.version ?? "missing")}
          ${renderChip("scrcpy", state.tools?.scrcpy.installed ?? false, state.tools?.scrcpy.version ?? "missing")}
          ${renderChip("approved fleet", true, `${state.approvedDevices.length} saved`)}
          <button data-action="bootstrap">Bootstrap Tools</button>
          <button data-action="refresh">Refresh</button>
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
  bindAction("bootstrap", async () => {
    pushLog("Bootstrapping local Android tools...");
    await post(`${apiRoot}/api/tools/bootstrap`);
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

  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-select]",
  )) {
    button.onclick = async () => {
      state.selectedSerial = button.dataset.select ?? "";
      await refreshPackages();
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
        `${apiRoot}/api/approved-devices/${encodeURIComponent(serial)}/approve`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/control`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/scrcpy`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/apps/start`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/apps/stop`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/tap`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/text`,
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
        `${apiRoot}/api/devices/${encodeURIComponent(state.selectedSerial)}/shell`,
        { command },
      );
      pushLog(
        `$ ${command}\n${result.stdout || result.stderr || "[no output]"}`,
      );
    };
  }
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
