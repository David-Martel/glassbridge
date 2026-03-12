import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const repoRoot = path.resolve(__dirname, "../../../");
export const vendorRoot = path.join(repoRoot, "vendor");
export const webDistRoot = path.join(repoRoot, "apps/web/dist");
export const dataRoot = path.join(repoRoot, "data");
export const host = process.env.HOST ?? "0.0.0.0";
export const port = Number(process.env.PORT ?? "3030");
