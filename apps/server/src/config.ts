// @deprecated — Replaced by apps/web/src-tauri/src/config.rs (native Rust config module).
// Retained for browser-mode development only (`npm run dev`).
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const repoRoot = path.resolve(__dirname, "../../../");
export const vendorRoot = path.join(repoRoot, "vendor");
export const webDistRoot = path.join(repoRoot, "apps/web/dist");
export const dataRoot =
  process.env.ANDROID_CONTROL_DATA_ROOT ?? path.join(repoRoot, "data");
export const host = process.env.HOST ?? "0.0.0.0";
export const port = Number(process.env.PORT ?? "3030");
