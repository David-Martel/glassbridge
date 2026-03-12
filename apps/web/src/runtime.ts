export type RuntimeMode = "browser" | "desktop";

export type RuntimeConfig = {
  apiRoot: string;
  wsUrl: string;
  mode: RuntimeMode;
  label: string;
  launchTarget: string;
};

type DesktopBootstrapPayload = {
  apiOrigin: string;
  wsUrl: string;
  launchTarget: string;
};

declare global {
  interface Window {
    __PIXEL_API_ORIGIN__?: string;
    __PIXEL_WS_URL__?: string;
    __TAURI_INTERNALS__?: unknown;
  }
}

export async function resolveRuntimeConfig(): Promise<RuntimeConfig> {
  if (window.__PIXEL_API_ORIGIN__ || window.__PIXEL_WS_URL__) {
    return {
      apiRoot: window.__PIXEL_API_ORIGIN__ ?? "",
      wsUrl: window.__PIXEL_WS_URL__ ?? defaultWsUrl(),
      mode: "browser",
      label: "Browser shell",
      launchTarget: window.location.origin,
    };
  }

  if (!window.__TAURI_INTERNALS__) {
    return {
      apiRoot: "",
      wsUrl: defaultWsUrl(),
      mode: "browser",
      label: "Browser shell",
      launchTarget: window.location.origin,
    };
  }

  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const payload = await invoke<DesktopBootstrapPayload>("desktop_bootstrap");

    return {
      apiRoot: payload.apiOrigin,
      wsUrl: payload.wsUrl,
      mode: "desktop",
      label: "Tauri desktop",
      launchTarget: payload.launchTarget,
    };
  } catch (error) {
    throw new Error(
      `Desktop bridge failed: ${error instanceof Error ? error.message : "Unknown startup failure"}`,
    );
  }
}

function defaultWsUrl() {
  return `${window.location.protocol === "https:" ? "wss" : "ws"}://${window.location.host}/ws`;
}
