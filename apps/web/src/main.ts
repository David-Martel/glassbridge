import "./styles.css";
import { type RuntimeConfig, resolveRuntimeConfig } from "./runtime";

// ─── Types ────────────────────────────────────────────────────────────────────

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

type DeviceProperties = {
  serial: string;
  model: string;
  manufacturer: string;
  androidVersion: string;
  sdkVersion: number;
  screenWidth: number;
  screenHeight: number;
  screenDensity: number;
  batteryLevel: number | null;
};

type TelemetrySummary = {
  totalOperations: number;
  totalErrors: number;
  avgLatencyMs: number;
  p95LatencyMs: number;
};

type WifiDevice = {
  address: string;
  serial: string;
  model: string | null;
  connected: boolean;
};

type LogcatEntry = {
  timestamp: string;
  pid: number;
  priority: string;
  tag: string;
  message: string;
};

type BottomTab = "terminal" | "logcat" | "eventlog" | "performance" | "help";

type TerminalLine = {
  kind: "cmd" | "out" | "err" | "info";
  text: string;
};

// ─── DOM root ─────────────────────────────────────────────────────────────────

const appEl = document.querySelector<HTMLDivElement>("#app");
if (!appEl) {
  throw new Error("Missing #app root");
}
const appRoot: HTMLDivElement = appEl;

// ─── Application state ────────────────────────────────────────────────────────

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
  screenScalePercent: 60,
  screenWidth: 0,
  screenHeight: 0,
  screenOrientation: "unknown" as ScreenOrientation,
  runtimeReady: false,
  runtimeLabel: "Launching desktop bridge",
  runtimeLaunchTarget: "",
  runtimeError: "",
  log: [] as string[],
  deviceProperties: null as DeviceProperties | null,
  telemetry: null as TelemetrySummary | null,
  wifiDevices: [] as WifiDevice[],
  logcatRunning: false,
  logcatEntries: [] as LogcatEntry[],
  // IDE layout state
  activeBottomTab: "terminal" as BottomTab,
  bottomHeight: 220,
  leftWidth: 240,
  rightWidth: 290,
  terminalLines: [] as TerminalLine[],
  terminalHistory: [] as string[],
  terminalHistoryIdx: -1,
  // Collapsed section state
  collapsedSections: new Set<string>(),
};

// ─── Runtime + timers ─────────────────────────────────────────────────────────

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

// ─── LLM terminal hook ────────────────────────────────────────────────────────

declare global {
  interface Window {
    __GLASSBRIDGE_TERMINAL__: {
      execute: (command: string) => Promise<string>;
      getHistory: () => string[];
      getOutput: () => string;
      inject: (command: string) => void;
    };
  }
}

window.__GLASSBRIDGE_TERMINAL__ = {
  execute: async (command: string): Promise<string> => {
    if (!state.selectedSerial) {
      return "[error: no device selected]";
    }
    try {
      const result = await post(
        apiUrl(
          `/api/devices/${encodeURIComponent(state.selectedSerial)}/shell`,
        ),
        { command },
      );
      const output: string = result.stdout || result.stderr || "[no output]";
      addTerminalLine("cmd", `$ ${command}`);
      addTerminalLine("out", output);
      return output;
    } catch (error) {
      const msg =
        error instanceof Error ? error.message : "Command execution failed";
      addTerminalLine("err", msg);
      return `[error: ${msg}]`;
    }
  },
  getHistory: () => [...state.terminalHistory],
  getOutput: () => state.terminalLines.map((line) => line.text).join("\n"),
  inject: (command: string) => {
    const input = document.querySelector<HTMLInputElement>(".terminal-input");
    if (input) {
      input.value = command;
      input.focus();
    }
  },
};

// ─── Constants (must be before render() call) ────────────────────────────────

const BOTTOM_TABS: Array<{ id: BottomTab; label: string }> = [
  { id: "terminal", label: "Terminal" },
  { id: "logcat", label: "Logcat" },
  { id: "eventlog", label: "Event Log" },
  { id: "performance", label: "Performance" },
  { id: "help", label: "Help" },
];

// ─── Boot ─────────────────────────────────────────────────────────────────────

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
    addTerminalLine("info", `Runtime ready — ${runtimeConfig.label}`);
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
    refreshWifiDevices(),
    refreshTelemetry(),
  ]);
}

// ─── Data refresh functions ───────────────────────────────────────────────────

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
    await refreshProperties();
  } else {
    state.packages = [];
    clearScreenCapture();
    state.deviceProperties = null;
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

async function refreshProperties() {
  if (!state.selectedSerial) {
    state.deviceProperties = null;
    render();
    return;
  }

  try {
    const response = await fetch(
      apiUrl(
        `/api/devices/${encodeURIComponent(state.selectedSerial)}/properties`,
      ),
    );
    if (!response.ok) {
      return;
    }
    const payload = await response.json();
    state.deviceProperties = payload;
    render();
  } catch {
    // Properties endpoint may not be available yet — silent no-op.
  }
}

async function refreshTelemetry() {
  try {
    const response = await fetch(apiUrl("/api/telemetry"));
    if (!response.ok) {
      return;
    }
    const payload = await response.json();
    const summary = payload.summary ?? payload;
    if (summary && typeof summary.totalOperations === "number") {
      state.telemetry = summary;
      render();
    }
  } catch {
    // Telemetry endpoint may not be available yet — silent no-op.
  }
}

async function refreshWifiDevices() {
  try {
    const response = await fetch(apiUrl("/api/wifi/devices"));
    if (!response.ok) {
      return;
    }
    const payload = await response.json();
    state.wifiDevices = payload.devices ?? [];
    render();
  } catch {
    // WiFi endpoint may not be available yet — silent no-op.
  }
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
      { cache: "no-store" },
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

// ─── WebSocket ────────────────────────────────────────────────────────────────

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

// ─── Render ───────────────────────────────────────────────────────────────────

function render() {
  const selected =
    state.devices.find((device) => device.serial === state.selectedSerial) ??
    null;

  appRoot.innerHTML = `
    <div class="app-shell" style="--left-width:${state.leftWidth}px; --right-width:${state.rightWidth}px; --bottom-height:${state.bottomHeight}px">
      ${renderToolbar()}
      <div class="work-area">
        ${state.runtimeError ? renderErrorBar() : ""}
        <div class="main-panels">
          ${renderLeftPanel()}
          ${renderCenterPanel(selected)}
          ${renderRightPanel(selected)}
          <div class="resize-handle-v resize-handle-v-left" data-resize="left"></div>
          <div class="resize-handle-v resize-handle-v-right" data-resize="right"></div>
        </div>
        ${renderBottomPanel()}
      </div>
    </div>
  `;

  bindEvents();
}

// ─── Toolbar ──────────────────────────────────────────────────────────────────

function renderToolbar() {
  const adbOk = state.tools?.adb.installed ?? false;
  const scrcpyOk = state.tools?.scrcpy.installed ?? false;
  const adbVer = state.tools?.adb.version ?? "missing";
  const scrcpyVer = state.tools?.scrcpy.version ?? "missing";
  const fleetCount = state.approvedDevices.length;

  return `
    <header class="toolbar-bar">
      <div class="toolbar-brand">
        <span class="toolbar-brand-name">GlassBridge</span>
      </div>
      <div class="toolbar-chips">
        <div class="status-chip ${state.runtimeReady ? "chip-ok" : "chip-err"}">
          <span class="chip-dot"></span>
          <span class="chip-label">Runtime</span>
          <span class="chip-value">${escapeHtml(state.runtimeLabel)}</span>
        </div>
        <div class="status-chip ${adbOk ? "chip-ok" : "chip-err"}">
          <span class="chip-dot"></span>
          <span class="chip-label">adb</span>
          <span class="chip-value">${escapeHtml(adbVer)}</span>
        </div>
        <div class="status-chip ${scrcpyOk ? "chip-ok" : "chip-err"}">
          <span class="chip-dot"></span>
          <span class="chip-label">scrcpy</span>
          <span class="chip-value">${escapeHtml(scrcpyVer)}</span>
        </div>
        <div class="status-chip chip-ok">
          <span class="chip-dot"></span>
          <span class="chip-label">fleet</span>
          <span class="chip-value">${fleetCount} saved</span>
        </div>
        <div class="status-chip chip-ok">
          <span class="chip-dot"></span>
          <span class="chip-label">devices</span>
          <span class="chip-value">${state.devices.length} connected</span>
        </div>
      </div>
      <div class="toolbar-actions">
        <button data-action="bootstrap" ${state.runtimeReady ? "" : "disabled"}>Bootstrap Tools</button>
        <button data-action="refresh" ${state.runtimeReady ? "" : "disabled"}>Refresh</button>
      </div>
    </header>
  `;
}

// ─── Error bar ────────────────────────────────────────────────────────────────

function renderErrorBar() {
  return `
    <div class="alert-bar">
      <strong>Desktop Launch Error:</strong>
      <span>${escapeHtml(state.runtimeError)}</span>
      <button data-action="retry-runtime">Retry</button>
    </div>
  `;
}

// ─── Left panel ───────────────────────────────────────────────────────────────

function renderLeftPanel() {
  return `
    <aside class="panel panel-left">
      <div class="panel-body">
        ${renderSection("devices", "Device Roster", `${state.devices.length}`, renderDeviceRoster())}
        ${renderSection("fleet", "Approved Fleet", `${state.approvedDevices.length}`, renderFleetList())}
        ${renderSection("scrcpy", "Live Mirror", "", renderScrcpyPresets())}
      </div>
    </aside>
  `;
}

function renderDeviceRoster() {
  if (state.devices.length === 0) {
    return `<p class="empty">No devices. Check USB debugging and RSA authorization.</p>`;
  }
  return state.devices.map(renderDeviceItem).join("");
}

function renderDeviceItem(device: DeviceRecord) {
  const isSelected = device.serial === state.selectedSerial;
  const displayName = device.alias ?? device.model ?? device.serial;
  return `
    <button class="device-item ${isSelected ? "selected" : ""}" data-select="${escapeHtml(device.serial)}">
      <div class="device-item-head">
        <span class="device-name">${escapeHtml(displayName)}</span>
        <span class="badge badge-${device.state}">${device.state}</span>
      </div>
      <span class="device-serial">${escapeHtml(device.serial)}</span>
      <span class="device-meta">${escapeHtml(device.product ?? "unknown")} / ${escapeHtml(device.device ?? "n/a")}</span>
      <div class="device-footer">
        <span class="badge ${device.approved ? "badge-approved" : "badge-candidate"}">${device.approved ? "approved" : "candidate"}</span>
        <button type="button" data-approve="${escapeHtml(device.serial)}" ${device.approved ? "disabled" : ""}>${device.approved ? "Saved" : "Approve"}</button>
      </div>
    </button>
  `;
}

function renderFleetList() {
  if (state.approvedDevices.length === 0) {
    return `<p class="empty">No approved devices saved yet.</p>`;
  }
  return state.approvedDevices
    .map(
      (device) => `
      <div class="fleet-item">
        <span class="fleet-name">${escapeHtml(device.alias ?? device.model ?? device.serial)}</span>
        <span class="fleet-serial">${escapeHtml(device.serial)}</span>
        <span class="fleet-time">${new Date(device.lastSeenAt).toLocaleString()}</span>
      </div>
    `,
    )
    .join("");
}

function renderScrcpyPresets() {
  const serial = state.selectedSerial;
  return `
    <div class="scrcpy-presets">
      <button data-scrcpy="latency" ${serial ? "" : "disabled"}>Low Latency</button>
      <button data-scrcpy="balanced" ${serial ? "" : "disabled"}>Balanced</button>
      <button data-scrcpy="quality" ${serial ? "" : "disabled"}>High Quality</button>
    </div>
    <p class="hint">Low-latency mirror via scrcpy. The API UI is for orchestration and fast device ops.</p>
  `;
}

// ─── Center panel ─────────────────────────────────────────────────────────────

function renderCenterPanel(selected: DeviceRecord | null) {
  const screenSummary = selected
    ? renderScreenSummary()
    : "Select a device from the roster";

  return `
    <div class="panel panel-center">
      <div class="center-toolbar">
        <span class="center-toolbar-title">Screen Preview</span>
        <span style="font-size:11px;color:var(--text-muted)">${escapeHtml(screenSummary)}</span>
        <span class="center-toolbar-spacer"></span>
        <button data-action="refresh-screen" ${selected ? "" : "disabled"}>
          ${state.screenCaptureLoading ? "Refreshing..." : "Refresh"}
        </button>
        <label class="toggle-label">
          <input type="checkbox" data-action="screen-auto-refresh" ${state.screenAutoRefresh ? "checked" : ""} ${selected ? "" : "disabled"} />
          <span>Auto</span>
        </label>
        <label class="scale-label">
          <input type="range" min="35" max="100" step="5" value="${state.screenScalePercent}" data-action="screen-scale" ${selected ? "" : "disabled"} />
          <span>${state.screenScalePercent}%</span>
        </label>
      </div>
      <div class="screen-viewport">
        ${
          state.screenCaptureUrl
            ? `
              <div class="screen-frame" style="padding:10px">
                <img
                  src="${state.screenCaptureUrl}"
                  alt="Device screen preview"
                  class="screen-preview"
                  data-screen-preview
                  style="max-width:${resolvePreviewWidth()}px;max-height:calc(100vh - 260px)"
                  draggable="false"
                />
              </div>
            `
            : `
              <div class="screen-empty">
                <div class="screen-empty-icon">&#9633;</div>
                <span>${selected ? "Click Refresh to load screen preview" : "No device selected"}</span>
              </div>
            `
        }
      </div>
    </div>
  `;
}

// ─── Right panel ──────────────────────────────────────────────────────────────

function renderRightPanel(selected: DeviceRecord | null) {
  const serial = selected?.serial ?? "";

  return `
    <div class="panel panel-right">
      <div class="panel-body">
        ${renderSection("controls", "Quick Controls", "", renderQuickControls(serial))}
        ${renderSection("tap", "Input", "", renderInputControls(serial))}
        ${renderSection("apps", "App Workspace", `${state.packages.length}`, renderAppWorkspace(serial))}
        ${renderSection("devinfo", "Device Info", "", renderDeviceInfo())}
        ${renderSection("wifi", "WiFi ADB", `${state.wifiDevices.length}`, renderWifiPanel(serial))}
      </div>
    </div>
  `;
}

function renderQuickControls(serial: string) {
  return `
    <div class="btn-grid">
      ${quickControls.map((item) => `<button data-control="${item.action}" ${serial ? "" : "disabled"}>${item.label}</button>`).join("")}
    </div>
  `;
}

function renderInputControls(serial: string) {
  return `
    <div class="field-group">
      <span class="field-label">Coordinate tap</span>
      <div class="input-row">
        <input name="tap-x" type="number" value="400" placeholder="X" ${serial ? "" : "disabled"} />
        <input name="tap-y" type="number" value="900" placeholder="Y" ${serial ? "" : "disabled"} />
        <button data-form-submit="tap" ${serial ? "" : "disabled"}>Tap</button>
      </div>
    </div>
    <div class="field-group">
      <span class="field-label">Send text</span>
      <div class="inline-row">
        <input name="send-text" type="text" placeholder="Type on the phone" ${serial ? "" : "disabled"} />
        <button data-form-submit="text" ${serial ? "" : "disabled"}>Send</button>
      </div>
    </div>
  `;
}

function renderAppWorkspace(serial: string) {
  return `
    <button data-action="packages" ${serial ? "" : "disabled"}>Refresh Packages</button>
    ${
      state.packages.length
        ? state.packages
            .map(
              (pkg) => `
            <div class="package-item">
              <span class="package-name">${escapeHtml(pkg)}</span>
              <div class="package-actions">
                <button data-launch="${escapeHtml(pkg)}" ${serial ? "" : "disabled"}>Launch</button>
                <button class="btn-danger" data-stop="${escapeHtml(pkg)}" ${serial ? "" : "disabled"}>Stop</button>
              </div>
            </div>
          `,
            )
            .join("")
        : `<p class="empty">No packages loaded yet.</p>`
    }
  `;
}

function renderDeviceInfo() {
  if (!state.deviceProperties) {
    return `
      <p class="empty">No properties loaded.</p>
      <button data-action="refresh-properties" ${state.selectedSerial ? "" : "disabled"}>Load Properties</button>
    `;
  }
  const props = state.deviceProperties;
  const rows: Array<[string, string]> = [
    ["Manufacturer", props.manufacturer],
    ["Model", props.model],
    ["Android", props.androidVersion],
    ["SDK", String(props.sdkVersion)],
    ["Resolution", `${props.screenWidth} x ${props.screenHeight}`],
    ["Density", `${props.screenDensity} dpi`],
    ["Battery", props.batteryLevel !== null ? `${props.batteryLevel}%` : "n/a"],
  ];
  return `
    <div class="props-table">
      ${rows
        .map(
          ([label, value]) => `
        <div class="props-row">
          <span class="props-key">${escapeHtml(label)}</span>
          <span class="props-val">${escapeHtml(value)}</span>
        </div>
      `,
        )
        .join("")}
    </div>
    <button data-action="refresh-properties" ${state.selectedSerial ? "" : "disabled"}>Refresh</button>
  `;
}

function renderWifiPanel(_serial: string) {
  return `
    <div class="field-group">
      <span class="field-label">Connect</span>
      <div class="inline-row">
        <input name="wifi-connect-addr" type="text" placeholder="192.168.1.100:5555" />
        <button data-form-submit="wifi-connect">Connect</button>
      </div>
    </div>
    <div class="field-group">
      <span class="field-label">Pair</span>
      <div class="inline-row">
        <input name="wifi-pair-addr" type="text" placeholder="192.168.1.100:5555" />
        <input name="wifi-pair-code" type="text" placeholder="Code" style="max-width:70px" />
        <button data-form-submit="wifi-pair">Pair</button>
      </div>
    </div>
    ${
      state.wifiDevices.length
        ? state.wifiDevices
            .map(
              (dev) => `
            <div class="wifi-item">
              <div class="wifi-info">
                <span class="wifi-model">${escapeHtml(dev.model ?? dev.serial)}</span>
                <span class="wifi-addr">${escapeHtml(dev.address)}</span>
              </div>
              <div style="display:flex;gap:5px;align-items:center">
                <span class="badge ${dev.connected ? "badge-approved" : "badge-candidate"}">${dev.connected ? "connected" : "offline"}</span>
                <button data-wifi-disconnect="${escapeHtml(dev.address)}" ${dev.connected ? "" : "disabled"}>Disconnect</button>
              </div>
            </div>
          `,
            )
            .join("")
        : `<p class="empty">No WiFi devices.</p>`
    }
  `;
}

// ─── Collapsible section widget ───────────────────────────────────────────────

function renderSection(
  id: string,
  title: string,
  badge: string,
  content: string,
) {
  const collapsed = state.collapsedSections.has(id);
  return `
    <div class="section ${collapsed ? "collapsed" : ""}" data-section="${id}">
      <div class="section-header" data-toggle-section="${id}">
        <span class="section-title">${title}</span>
        ${badge ? `<span class="section-badge">${badge}</span>` : ""}
        <span class="section-chevron">&#9660;</span>
      </div>
      <div class="section-body">
        ${content}
      </div>
    </div>
  `;
}

// ─── Bottom panel ─────────────────────────────────────────────────────────────

function renderBottomPanel() {
  return `
    <div class="bottom-panel" style="height:${state.bottomHeight}px" id="bottom-panel">
      <div class="bottom-resize-handle" data-resize="bottom"></div>
      <div class="bottom-tab-bar">
        ${BOTTOM_TABS.map((tab) => `<button class="bottom-tab ${state.activeBottomTab === tab.id ? "active" : ""}" data-tab="${tab.id}">${tab.label}</button>`).join("")}
      </div>
      <div class="bottom-tab-content">
        <div class="tab-pane ${state.activeBottomTab === "terminal" ? "active" : ""}" id="tab-terminal">
          ${renderTerminalTab()}
        </div>
        <div class="tab-pane ${state.activeBottomTab === "logcat" ? "active" : ""}" id="tab-logcat">
          <div class="logcat-placeholder">
            <strong>Logcat streaming</strong>
            <span>Connects to /api/devices/{serial}/logcat/start — coming in next build</span>
          </div>
        </div>
        <div class="tab-pane ${state.activeBottomTab === "eventlog" ? "active" : ""}" id="tab-eventlog">
          ${renderEventLogTab()}
        </div>
        <div class="tab-pane ${state.activeBottomTab === "performance" ? "active" : ""}" id="tab-performance">
          ${renderPerformanceTab()}
        </div>
        <div class="tab-pane ${state.activeBottomTab === "help" ? "active" : ""}" id="tab-help">
          ${renderHelpTab()}
        </div>
      </div>
    </div>
  `;
}

function renderTerminalTab() {
  const lines = state.terminalLines
    .map(
      (line) =>
        `<div class="terminal-line terminal-line-${line.kind}">${escapeHtml(line.text)}</div>`,
    )
    .join("");

  return `
    <div class="terminal-output" id="terminal-output">${lines}</div>
    <div class="terminal-input-bar">
      <span class="terminal-prompt">$</span>
      <input
        class="terminal-input"
        id="terminal-input"
        type="text"
        autocomplete="off"
        autocorrect="off"
        spellcheck="false"
        placeholder="${state.selectedSerial ? `shell on ${state.selectedSerial}` : "select a device to use the terminal"}"
        ${state.selectedSerial ? "" : "disabled"}
      />
    </div>
  `;
}

function renderEventLogTab() {
  if (state.log.length === 0) {
    return `<div class="log-list"><p class="empty">No events yet.</p></div>`;
  }
  return `
    <div class="log-list">
      ${state.log.map((entry) => `<div class="log-entry">${escapeHtml(entry)}</div>`).join("")}
    </div>
  `;
}

function renderPerformanceTab() {
  if (!state.telemetry) {
    return `
      <div class="perf-grid">
        <p class="empty">No telemetry data.</p>
      </div>
      <div style="padding:10px">
        <button data-action="refresh-telemetry">Refresh Telemetry</button>
      </div>
    `;
  }
  const t = state.telemetry;
  return `
    <div class="perf-grid">
      <div class="perf-stat">
        <div class="perf-stat-label">Total ops</div>
        <div class="perf-stat-value">${t.totalOperations.toLocaleString()}</div>
      </div>
      <div class="perf-stat">
        <div class="perf-stat-label">Errors</div>
        <div class="perf-stat-value" style="color:${t.totalErrors > 0 ? "var(--text-danger)" : "var(--text-accent)"}">${t.totalErrors.toLocaleString()}</div>
      </div>
      <div class="perf-stat">
        <div class="perf-stat-label">Avg latency</div>
        <div class="perf-stat-value">${t.avgLatencyMs.toFixed(1)}<span style="font-size:12px;font-weight:400"> ms</span></div>
      </div>
      <div class="perf-stat">
        <div class="perf-stat-label">p95 latency</div>
        <div class="perf-stat-value">${t.p95LatencyMs.toFixed(1)}<span style="font-size:12px;font-weight:400"> ms</span></div>
      </div>
    </div>
    <div style="padding:0 10px 10px">
      <button data-action="refresh-telemetry">Refresh Telemetry</button>
    </div>
  `;
}

function renderHelpTab() {
  if (state.helpTopics.length === 0) {
    return `<div class="help-topics"><p class="empty">No help topics loaded.</p></div>`;
  }
  return `
    <div class="help-topics">
      ${state.helpTopics
        .map(
          (topic) => `
        <div class="help-topic">
          <h3>${escapeHtml(topic.title)}</h3>
          <ul>
            ${topic.bullets.map((bullet) => `<li>${escapeHtml(bullet)}</li>`).join("")}
          </ul>
        </div>
      `,
        )
        .join("")}
    </div>
  `;
}

// ─── Event binding ────────────────────────────────────────────────────────────

function bindEvents() {
  // Toolbar actions
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

  bindAction("refresh-properties", async () => {
    await refreshProperties();
    pushLog(`Refreshed device properties for ${state.selectedSerial}`);
  });

  bindAction("refresh-telemetry", async () => {
    await refreshTelemetry();
    pushLog("Refreshed performance telemetry.");
  });

  // Bottom tabs
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-tab]",
  )) {
    button.onclick = () => {
      state.activeBottomTab = (button.dataset.tab ?? "terminal") as BottomTab;
      render();
    };
  }

  // Collapsible sections
  for (const header of document.querySelectorAll<HTMLElement>(
    "[data-toggle-section]",
  )) {
    header.onclick = () => {
      const sectionId = header.dataset.toggleSection ?? "";
      if (state.collapsedSections.has(sectionId)) {
        state.collapsedSections.delete(sectionId);
      } else {
        state.collapsedSections.add(sectionId);
      }
      render();
    };
  }

  // Device selection
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-select]",
  )) {
    button.onclick = async () => {
      state.selectedSerial = button.dataset.select ?? "";
      await refreshPackages();
      await refreshScreenCapture();
      await refreshProperties();
      render();
    };
  }

  // Approve device
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-approve]",
  )) {
    button.onclick = async (event) => {
      event.stopPropagation();
      const serial = button.dataset.approve ?? "";
      const device = state.devices.find((entry) => entry.serial === serial);
      await post(
        apiUrl(`/api/approved-devices/${encodeURIComponent(serial)}/approve`),
        { alias: device?.alias ?? device?.model ?? undefined },
      );
      await refreshApprovedDevices();
      await refreshDevices();
      pushLog(`Approved ${serial} for future reuse.`);
    };
  }

  // Quick controls
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

  // scrcpy presets
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

  // App launch / stop
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

  // WiFi disconnect
  for (const button of document.querySelectorAll<HTMLButtonElement>(
    "[data-wifi-disconnect]",
  )) {
    button.onclick = async () => {
      const address = button.dataset.wifiDisconnect ?? "";
      await post(apiUrl("/api/wifi/disconnect"), { address });
      await refreshWifiDevices();
      pushLog(`WiFi disconnected: ${address}`);
    };
  }

  // Inline form submits (tap, text, wifi-connect, wifi-pair)
  const tapX = document.querySelector<HTMLInputElement>("[name='tap-x']");
  const tapY = document.querySelector<HTMLInputElement>("[name='tap-y']");
  const tapBtn = document.querySelector<HTMLButtonElement>(
    "[data-form-submit='tap']",
  );
  if (tapBtn && tapX && tapY) {
    tapBtn.onclick = async () => {
      const x = Number(tapX.value);
      const y = Number(tapY.value);
      await post(
        apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/tap`),
        { x, y },
      );
      pushLog(`Tapped ${x},${y}`);
    };
  }

  const sendTextInput =
    document.querySelector<HTMLInputElement>("[name='send-text']");
  const sendTextBtn = document.querySelector<HTMLButtonElement>(
    "[data-form-submit='text']",
  );
  if (sendTextBtn && sendTextInput) {
    sendTextBtn.onclick = async () => {
      const text = sendTextInput.value;
      if (!text) {
        return;
      }
      await post(
        apiUrl(`/api/devices/${encodeURIComponent(state.selectedSerial)}/text`),
        { text },
      );
      pushLog(`Sent text to ${state.selectedSerial}`);
      sendTextInput.value = "";
    };
  }

  const wifiConnectAddr = document.querySelector<HTMLInputElement>(
    "[name='wifi-connect-addr']",
  );
  const wifiConnectBtn = document.querySelector<HTMLButtonElement>(
    "[data-form-submit='wifi-connect']",
  );
  if (wifiConnectBtn && wifiConnectAddr) {
    wifiConnectBtn.onclick = async () => {
      const address = wifiConnectAddr.value.trim();
      if (!address) {
        return;
      }
      await post(apiUrl("/api/wifi/connect"), { address });
      await refreshWifiDevices();
      pushLog(`WiFi connect requested: ${address}`);
      wifiConnectAddr.value = "";
    };
  }

  const wifiPairAddr = document.querySelector<HTMLInputElement>(
    "[name='wifi-pair-addr']",
  );
  const wifiPairCode = document.querySelector<HTMLInputElement>(
    "[name='wifi-pair-code']",
  );
  const wifiPairBtn = document.querySelector<HTMLButtonElement>(
    "[data-form-submit='wifi-pair']",
  );
  if (wifiPairBtn && wifiPairAddr && wifiPairCode) {
    wifiPairBtn.onclick = async () => {
      const address = wifiPairAddr.value.trim();
      const code = wifiPairCode.value.trim();
      if (!address || !code) {
        return;
      }
      await post(apiUrl("/api/wifi/pair"), { address, code });
      await refreshWifiDevices();
      pushLog(`WiFi pair requested: ${address}`);
      wifiPairAddr.value = "";
      wifiPairCode.value = "";
    };
  }

  // Screen controls
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

  const screenScale = document.querySelector<HTMLInputElement>(
    "[data-action='screen-scale']",
  );
  if (screenScale) {
    screenScale.oninput = () => {
      state.screenScalePercent = Number(screenScale.value);
      render();
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
      if (!state.selectedSerial || !screenPreview.naturalWidth) {
        return;
      }
      const rect = screenPreview.getBoundingClientRect();
      const scaleX = screenPreview.naturalWidth / rect.width;
      const scaleY = screenPreview.naturalHeight / rect.height;
      const rawX = Math.round((event.clientX - rect.left) * scaleX);
      const rawY = Math.round((event.clientY - rect.top) * scaleY);
      const x = clamp(rawX, 0, screenPreview.naturalWidth - 1);
      const y = clamp(rawY, 0, screenPreview.naturalHeight - 1);
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

  // Terminal input
  bindTerminal();

  // Panel resize drag handles
  bindResizeHandles();
}

// ─── Terminal binding ─────────────────────────────────────────────────────────

function bindTerminal() {
  const input = document.querySelector<HTMLInputElement>("#terminal-input");
  const output = document.querySelector<HTMLElement>("#terminal-output");

  if (!input) {
    return;
  }

  // Scroll terminal output to bottom on render
  if (output) {
    output.scrollTop = output.scrollHeight;
  }

  input.onkeydown = async (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      const command = input.value.trim();
      if (!command) {
        return;
      }
      input.value = "";

      // Save to history
      state.terminalHistory = [
        command,
        ...state.terminalHistory.filter((c) => c !== command),
      ].slice(0, 100);
      state.terminalHistoryIdx = -1;

      await window.__GLASSBRIDGE_TERMINAL__.execute(command);
      render();
      // Re-focus after render
      const newInput =
        document.querySelector<HTMLInputElement>("#terminal-input");
      newInput?.focus();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      const maxIdx = state.terminalHistory.length - 1;
      state.terminalHistoryIdx = clamp(state.terminalHistoryIdx + 1, 0, maxIdx);
      input.value = state.terminalHistory[state.terminalHistoryIdx] ?? "";
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      state.terminalHistoryIdx = Math.max(state.terminalHistoryIdx - 1, -1);
      input.value =
        state.terminalHistoryIdx >= 0
          ? (state.terminalHistory[state.terminalHistoryIdx] ?? "")
          : "";
    }
  };
}

// ─── Panel resize drag handles ────────────────────────────────────────────────

function bindResizeHandles() {
  // Bottom panel vertical resize
  const bottomHandle = document.querySelector<HTMLElement>(
    "[data-resize='bottom']",
  );
  if (bottomHandle) {
    bottomHandle.onmousedown = (startEvent) => {
      startEvent.preventDefault();
      bottomHandle.classList.add("dragging");
      const startY = startEvent.clientY;
      const startHeight = state.bottomHeight;

      const onMove = (event: MouseEvent) => {
        const delta = startY - event.clientY;
        state.bottomHeight = clamp(
          startHeight + delta,
          150,
          window.innerHeight * 0.5,
        );
        const panel = document.getElementById("bottom-panel");
        if (panel) {
          panel.style.height = `${state.bottomHeight}px`;
        }
      };

      const onUp = () => {
        bottomHandle.classList.remove("dragging");
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
      };

      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
    };
  }

  // Left panel horizontal resize
  const leftHandle = document.querySelector<HTMLElement>(
    "[data-resize='left']",
  );
  if (leftHandle) {
    leftHandle.onmousedown = (startEvent) => {
      startEvent.preventDefault();
      leftHandle.classList.add("dragging");
      const startX = startEvent.clientX;
      const startWidth = state.leftWidth;

      const onMove = (event: MouseEvent) => {
        const delta = event.clientX - startX;
        state.leftWidth = clamp(startWidth + delta, 160, 400);
        const shell = document.querySelector<HTMLElement>(".app-shell");
        if (shell) {
          shell.style.setProperty("--left-width", `${state.leftWidth}px`);
        }
        const handle = document.querySelector<HTMLElement>(
          ".resize-handle-v-left",
        );
        if (handle) {
          handle.style.left = `calc(${state.leftWidth}px - 3px)`;
        }
      };

      const onUp = () => {
        leftHandle.classList.remove("dragging");
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
      };

      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
    };
  }

  // Right panel horizontal resize
  const rightHandle = document.querySelector<HTMLElement>(
    "[data-resize='right']",
  );
  if (rightHandle) {
    rightHandle.onmousedown = (startEvent) => {
      startEvent.preventDefault();
      rightHandle.classList.add("dragging");
      const startX = startEvent.clientX;
      const startWidth = state.rightWidth;

      const onMove = (event: MouseEvent) => {
        const delta = startX - event.clientX;
        state.rightWidth = clamp(startWidth + delta, 200, 500);
        const shell = document.querySelector<HTMLElement>(".app-shell");
        if (shell) {
          shell.style.setProperty("--right-width", `${state.rightWidth}px`);
        }
        const handle = document.querySelector<HTMLElement>(
          ".resize-handle-v-right",
        );
        if (handle) {
          handle.style.right = `calc(${state.rightWidth}px - 3px)`;
        }
      };

      const onUp = () => {
        rightHandle.classList.remove("dragging");
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
      };

      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
    };
  }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

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
    headers: { "Content-Type": "application/json" },
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
  ].slice(0, 40);
  // If eventlog tab is active, partial re-render; else just update state
  render();
}

function addTerminalLine(kind: TerminalLine["kind"], text: string) {
  state.terminalLines = [...state.terminalLines, { kind, text }].slice(-500);
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
  return `${formatScreenOrientation(state.screenOrientation)} — ${state.screenWidth} x ${state.screenHeight}`;
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

function resolvePreviewWidth() {
  if (!state.screenWidth) {
    return 380;
  }
  const scaledWidth = Math.round(
    state.screenWidth * (state.screenScalePercent / 100),
  );
  const minimum = state.screenOrientation === "landscape" ? 360 : 240;
  const maximum = state.screenOrientation === "landscape" ? 900 : 520;
  return clamp(scaledWidth, minimum, maximum);
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(Math.max(value, minimum), maximum);
}

function escapeHtml(value: string) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}
