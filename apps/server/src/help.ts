// @deprecated — Replaced by apps/web/src-tauri/src/help.rs (native Rust help module).
// Retained for browser-mode development only (`npm run dev`).
export const helpTopics = [
  {
    id: "connect",
    title: "Connect A Device",
    bullets: [
      "Enable Developer Options and USB debugging on the phone.",
      "Accept the RSA fingerprint prompt the first time the workstation connects.",
      "If the device shows unauthorized, reconnect USB and re-accept the prompt.",
    ],
  },
  {
    id: "realtime",
    title: "Real-Time Control",
    bullets: [
      "Use scrcpy for low-latency visual control instead of repeated screenshots.",
      "Low Latency trades image detail for gesture response.",
      "High Quality is better for demos and visual inspection than active driving.",
    ],
  },
  {
    id: "apps",
    title: "App Operations",
    bullets: [
      "Refresh packages to pull current third-party apps from the selected device.",
      "Launch starts the default launcher activity with monkey.",
      "Force Stop is useful for repeatable cold-start testing.",
    ],
  },
  {
    id: "recovery",
    title: "Recovery",
    bullets: [
      "If the device is offline, restart adb and reconnect the cable.",
      "If binaries are missing, run the local bootstrap action.",
      "Keep only one adb server active on the workstation to avoid contention.",
    ],
  },
] as const;
