// @deprecated — Replaced by apps/web/src-tauri/src/tooling.rs (native Rust tooling module).
// Retained for browser-mode development only (`npm run dev`).
import { spawn } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import path from "node:path";
import { repoRoot, vendorRoot } from "./config.js";

type ToolName = "adb" | "scrcpy";
type ManifestEntry = {
  executablePath?: string;
  url?: string;
  verifiedAt?: string;
};

type ToolManifest = Partial<Record<ToolName, ManifestEntry>>;

export async function getToolStatus() {
  const manifest = await readToolManifest();
  const adbPath = await resolveToolPath("adb", manifest);
  const scrcpyPath = await resolveToolPath("scrcpy", manifest);

  const adbInstalled = !!adbPath && (await exists(adbPath));
  const scrcpyInstalled = !!scrcpyPath && (await exists(scrcpyPath));

  return {
    vendorRoot,
    adb: {
      installed: adbInstalled,
      path: adbPath,
      version:
        adbInstalled && adbPath
          ? await readVersion(adbPath, ["version"])
          : null,
    },
    scrcpy: {
      installed: scrcpyInstalled,
      path: scrcpyPath,
      version:
        scrcpyInstalled && scrcpyPath
          ? await readVersion(scrcpyPath, ["--version"])
          : null,
    },
  };
}

export async function getAdbPath() {
  const manifest = await readToolManifest();
  const resolved = await resolveToolPath("adb", manifest);
  if (!resolved) {
    throw new Error("adb is not installed locally. Run bootstrap first.");
  }
  return resolved;
}

export async function getScrcpyPath() {
  const manifest = await readToolManifest();
  const resolved = await resolveToolPath("scrcpy", manifest);
  if (!resolved) {
    throw new Error("scrcpy is not installed locally. Run bootstrap first.");
  }
  return resolved;
}

export async function bootstrapTools() {
  return runCommand(
    "node",
    [path.join(repoRoot, "tools/bootstrap-tools.mjs")],
    repoRoot,
  );
}

export async function readToolManifest(): Promise<ToolManifest | null> {
  try {
    const raw = await readFile(
      path.join(vendorRoot, "tool-manifest.json"),
      "utf8",
    );
    return JSON.parse(raw) as ToolManifest;
  } catch {
    return null;
  }
}

export async function runCommand(command: string, args: string[], cwd: string) {
  return new Promise<{ stdout: string; stderr: string; exitCode: number }>(
    (resolve, reject) => {
      const child = spawn(command, args, {
        cwd,
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
      });

      let stdout = "";
      let stderr = "";

      child.stdout.on("data", (chunk) => {
        stdout += chunk.toString();
      });
      child.stderr.on("data", (chunk) => {
        stderr += chunk.toString();
      });
      child.on("error", reject);
      child.on("close", (exitCode) => {
        if (exitCode && exitCode !== 0) {
          reject(
            new Error(
              stderr || stdout || `${command} exited with code ${exitCode}`,
            ),
          );
          return;
        }
        resolve({ stdout, stderr, exitCode: exitCode ?? 0 });
      });
    },
  );
}

export async function runCommandBuffer(
  command: string,
  args: string[],
  cwd: string,
) {
  return new Promise<{ stdout: Buffer; stderr: string; exitCode: number }>(
    (resolve, reject) => {
      const child = spawn(command, args, {
        cwd,
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
      });

      const stdoutChunks: Buffer[] = [];
      let stderr = "";

      child.stdout.on("data", (chunk: Buffer) => {
        stdoutChunks.push(chunk);
      });
      child.stderr.on("data", (chunk) => {
        stderr += chunk.toString();
      });
      child.on("error", reject);
      child.on("close", (exitCode) => {
        if (exitCode && exitCode !== 0) {
          reject(
            new Error(stderr || `${command} exited with code ${exitCode}`),
          );
          return;
        }
        resolve({
          stdout: Buffer.concat(stdoutChunks),
          stderr,
          exitCode: exitCode ?? 0,
        });
      });
    },
  );
}

async function resolveToolPath(tool: ToolName, manifest: ToolManifest | null) {
  const fromManifest = manifest?.[tool]?.executablePath;
  if (fromManifest && (await exists(fromManifest))) {
    return fromManifest;
  }

  const fallbackMap: Record<ToolName, string[]> = {
    adb: [
      path.join(vendorRoot, "platform-tools", "platform-tools", "adb.exe"),
      path.join(vendorRoot, "platform-tools", "adb.exe"),
    ],
    scrcpy: [
      path.join(vendorRoot, "scrcpy", "scrcpy-win64-v3.3.4", "scrcpy.exe"),
      path.join(vendorRoot, "scrcpy", "scrcpy.exe"),
    ],
  };

  for (const candidate of fallbackMap[tool]) {
    if (await exists(candidate)) {
      return candidate;
    }
  }

  return null;
}

async function exists(filePath: string) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}

async function readVersion(command: string, args: string[]) {
  const result = await runCommand(command, args, repoRoot);
  const lines = `${result.stdout}\n${result.stderr}`
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  return lines[0] ?? "unknown";
}
