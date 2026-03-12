import { spawn } from "node:child_process";
import { repoRoot } from "./config.js";
import {
  getAdbPath,
  getScrcpyPath,
  runCommand,
  runCommandBuffer,
} from "./tooling.js";

export type DeviceRecord = {
  serial: string;
  state: string;
  model: string | null;
  product: string | null;
  device: string | null;
  transportId: string | null;
};

const keyMap: Record<string, string> = {
  home: "KEYCODE_HOME",
  back: "KEYCODE_BACK",
  recents: "KEYCODE_APP_SWITCH",
  power: "KEYCODE_POWER",
  wake: "KEYCODE_WAKEUP",
  volumeUp: "KEYCODE_VOLUME_UP",
  volumeDown: "KEYCODE_VOLUME_DOWN",
};

export async function listDevices() {
  const adbPath = await getAdbPath();
  const result = await runCommand(adbPath, ["devices", "-l"], repoRoot);

  return result.stdout
    .split(/\r?\n/)
    .slice(1)
    .map((line) => line.trim())
    .filter(Boolean)
    .map(parseDeviceLine);
}

export async function listPackages(serial: string) {
  const result = await runAdb(serial, [
    "shell",
    "pm",
    "list",
    "packages",
    "-3",
  ]);
  return result.stdout
    .split(/\r?\n/)
    .map((line) => line.replace(/^package:/, "").trim())
    .filter(Boolean)
    .sort((a, b) => a.localeCompare(b));
}

export async function sendKey(serial: string, action: string) {
  const keyCode = keyMap[action];
  if (!keyCode) {
    throw new Error(`Unsupported action: ${action}`);
  }
  return runAdb(serial, ["shell", "input", "keyevent", keyCode]);
}

export async function tap(serial: string, x: number, y: number) {
  return runAdb(serial, ["shell", "input", "tap", String(x), String(y)]);
}

export async function inputText(serial: string, text: string) {
  return runAdb(serial, ["shell", "input", "text", text.replace(/ /g, "%s")]);
}

export async function launchPackage(serial: string, packageName: string) {
  return runAdb(serial, [
    "shell",
    "monkey",
    "-p",
    packageName,
    "-c",
    "android.intent.category.LAUNCHER",
    "1",
  ]);
}

export async function stopPackage(serial: string, packageName: string) {
  return runAdb(serial, ["shell", "am", "force-stop", packageName]);
}

export async function shellCommand(serial: string, command: string) {
  return runAdb(serial, ["shell", command]);
}

export async function captureScreenshot(serial: string) {
  const adbPath = await getAdbPath();
  const result = await runCommandBuffer(
    adbPath,
    ["-s", serial, "exec-out", "screencap", "-p"],
    repoRoot,
  );

  return result.stdout;
}

export async function startScrcpy(
  serial: string,
  preset: "latency" | "balanced" | "quality",
) {
  const scrcpyPath = await getScrcpyPath();
  const args = ["-s", serial];

  if (preset === "latency") {
    args.push(
      "--video-bit-rate=8M",
      "--max-fps=60",
      "--max-size=1280",
      "--no-audio",
    );
  } else if (preset === "quality") {
    args.push("--video-bit-rate=24M", "--max-fps=60", "--max-size=2160");
  } else {
    args.push("--video-bit-rate=12M", "--max-fps=60", "--max-size=1600");
  }

  spawn(scrcpyPath, args, {
    cwd: repoRoot,
    detached: true,
    stdio: "ignore",
    windowsHide: false,
  }).unref();

  return { ok: true, preset };
}

async function runAdb(serial: string, args: string[]) {
  const adbPath = await getAdbPath();
  return runCommand(adbPath, ["-s", serial, ...args], repoRoot);
}

function parseDeviceLine(line: string): DeviceRecord {
  const [serial = "", state = "", ...rest] = line.split(/\s+/);
  const metadata = new Map(
    rest
      .filter((part) => part.includes(":"))
      .map((part) => {
        const [key, value] = part.split(":");
        return [key, value];
      }),
  );

  return {
    serial,
    state,
    model: metadata.get("model")?.replaceAll("_", " ") ?? null,
    product: metadata.get("product")?.replaceAll("_", " ") ?? null,
    device: metadata.get("device")?.replaceAll("_", " ") ?? null,
    transportId: metadata.get("transport_id") ?? null,
  };
}
