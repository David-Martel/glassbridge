import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { stat } from "node:fs/promises";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const vendorRoot = path.join(repoRoot, "vendor");
const cacheRoot = path.join(vendorRoot, "cache");
const manifestPath = path.join(vendorRoot, "tool-manifest.json");
const isVerifyOnly = process.argv.includes("--verify");

const tools = [
  {
    key: "adb",
    url: "https://dl.google.com/android/repository/platform-tools-latest-windows.zip",
    archiveName: "platform-tools-latest-windows.zip",
    extractDir: "platform-tools",
    executableName: "adb.exe",
  },
  {
    key: "scrcpy",
    url: "https://github.com/Genymobile/scrcpy/releases/download/v3.3.4/scrcpy-win64-v3.3.4.zip",
    archiveName: "scrcpy-win64-v3.3.4.zip",
    extractDir: "scrcpy",
    executableName: "scrcpy.exe",
  },
];

mkdirSync(vendorRoot, { recursive: true });
mkdirSync(cacheRoot, { recursive: true });

const manifest = {};

for (const tool of tools) {
  const archivePath = path.join(cacheRoot, tool.archiveName);
  const installRoot = path.join(vendorRoot, tool.extractDir);

  if (!isVerifyOnly) {
    if (!existsSync(archivePath)) {
      console.log(`Downloading ${tool.key} from ${tool.url}`);
      const response = await fetch(tool.url);
      if (!response.ok || !response.body) {
        throw new Error(
          `Failed to download ${tool.key}: ${response.status} ${response.statusText}`,
        );
      }
      await pipeline(
        Readable.fromWeb(response.body),
        createWriteStream(archivePath),
      );
    } else {
      console.log(`Using cached archive for ${tool.key}`);
    }

    removePath(installRoot);
    mkdirSync(installRoot, { recursive: true });
    expandArchive(archivePath, installRoot);
  }

  const executablePath = resolveExecutable(installRoot, tool.executableName);
  if (!executablePath) {
    throw new Error(`Missing ${tool.executableName} inside ${installRoot}`);
  }

  manifest[tool.key] = {
    url: tool.url,
    archivePath,
    archiveSha256: await sha256File(archivePath),
    executablePath,
    executableBytes: (await stat(executablePath)).size,
    verifiedAt: new Date().toISOString(),
  };
}

writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
console.log(`Wrote ${manifestPath}`);

function expandArchive(archivePath, destinationPath) {
  execFileSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      `Expand-Archive -LiteralPath '${escapeForPowerShell(archivePath)}' -DestinationPath '${escapeForPowerShell(destinationPath)}' -Force`,
    ],
    { stdio: "inherit" },
  );
}

function removePath(targetPath) {
  try {
    rmSync(targetPath, { recursive: true, force: true });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!message.includes("EPERM")) {
      throw error;
    }

    execFileSync(
      "powershell",
      [
        "-NoProfile",
        "-Command",
        "if (Test-Path $args[0]) { Remove-Item -LiteralPath $args[0] -Recurse -Force -ErrorAction Stop }",
        targetPath,
      ],
      { stdio: "inherit" },
    );
  }
}

function resolveExecutable(root, executableName) {
  const entries = readdirSync(root, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(root, entry.name);
    if (
      entry.isFile() &&
      entry.name.toLowerCase() === executableName.toLowerCase()
    ) {
      return fullPath;
    }
    if (entry.isDirectory()) {
      const nested = resolveExecutable(fullPath, executableName);
      if (nested) {
        return nested;
      }
    }
  }
  return null;
}

async function sha256File(filePath) {
  const { createReadStream } = await import("node:fs");
  const hash = createHash("sha256");
  await pipeline(createReadStream(filePath), hash);
  return hash.digest("hex");
}

function escapeForPowerShell(value) {
  return value.replaceAll("'", "''");
}
