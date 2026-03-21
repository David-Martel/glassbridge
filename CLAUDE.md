# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GlassBridge is a local-first Android device access framework for Windows desktop operators. It combines a Fastify control plane, a Vite browser UI, and a Tauri desktop shell around real `adb` and `scrcpy` workflows to manage multiple approved devices concurrently.

## Build and Development Commands

```bash
# Initial setup
npm install
npm run bootstrap:tools   # Downloads adb + scrcpy into vendor/
npm run hooks:install      # Installs pre-commit and pre-push git hooks

# Development (runs server + web concurrently)
npm run dev                # Server at :3030, Web at :5173

# Desktop shell (Windows only, requires Rust toolchain)
npm run desktop            # Full desktop launch via Launch-PixelDesktop.ps1
npm run desktop:check      # Tauri/Rust check + cargo build --release
npm run desktop:build      # Build and stage to staging/desktop/

# Build
npm run build              # Build both server and web workspaces

# Lint and typecheck
npm run lint               # Biome check (linting + formatting)
npm run typecheck          # TypeScript --noEmit on both workspaces
```

## Testing

```bash
npm test                   # Full suite: build + frontend + backend + integration

# Individual test suites
npm run test:frontend      # vitest - DOM/UI tests (no device needed)
npm run test:backend       # node --test - live device smoke tests (device required)
npm run test:integration   # vitest - frontend/backend contract tests (device required)
npm run test:desktop       # build server + cargo check + cargo build
npm run test:ci            # build + frontend only (CI-safe, no device)

# Run a single frontend test file
npx vitest run tests/frontend-ui.test.ts

# Playwright (requires device + built assets)
npm run test:playwright:headed
```

Frontend tests use vitest with jsdom environment. Backend tests use Node's built-in test runner. Integration tests spin up the real Fastify server and seed the frontend with live device payloads.

## Validation and Security

```bash
npm run validate:staged    # Pre-commit: secrets + lint + typecheck + frontend tests + desktop check
npm run validate:release   # Pre-push: full secret scan + lint + typecheck + all tests + desktop

npm run secrets:staged     # gitleaks on staged content
npm run secrets:repo       # gitleaks on current repo
npm run secrets:history    # gitleaks on full git history
```

Git hooks (`.githooks/`) run `validate:staged` on commit and `validate:release` on push. Install with `npm run hooks:install`.

## Architecture

### npm Workspaces

The repo is an npm workspace monorepo with two packages:

- **`apps/server`** (glassbridge-server) - Fastify backend with TypeScript, built via `tsc`, run via `tsx` in dev
- **`apps/web`** (glassbridge-web) - Vite frontend with vanilla TypeScript, also contains the Tauri desktop shell

### Server (`apps/server/src/`)

| File | Role |
|------|------|
| `index.ts` | Fastify app setup, all REST/WS routes, WebSocket snapshot broadcast (2s interval) |
| `adb.ts` | All adb operations: device listing, screenshots, key events, tap, text input, shell, scrcpy launch |
| `registry.ts` | SQLite device registry via `node:sqlite` (`DatabaseSync`). Tables: `device_registry`, `device_sessions` |
| `tooling.ts` | Tool path resolution (adb/scrcpy from vendor manifest or fallback paths), `runCommand`/`runCommandBuffer` helpers |
| `config.ts` | Repo root, vendor root, data root, host/port from env vars |
| `help.ts` | Static operator help content |

Request validation uses **Zod** schemas inline in route handlers. The server uses **ESM** (`"type": "module"`) with `NodeNext` module resolution.

### Frontend (`apps/web/src/`)

| File | Role |
|------|------|
| `main.ts` | Full UI: state management, rendering (innerHTML-based), event binding, WebSocket connection, screen preview with click-to-tap |
| `runtime.ts` | Runtime detection: browser mode (direct/proxied) vs Tauri desktop mode (invokes `desktop_bootstrap` command) |
| `styles.css` | All styles |

The frontend is vanilla TypeScript with no framework. UI updates call `render()` which rebuilds `innerHTML` then calls `bindEvents()` to attach handlers. Vite proxies `/api` and `/ws` to the backend in dev mode.

### Desktop Shell (`apps/web/src-tauri/`)

Tauri v2 app in Rust. The `main.rs`:
- Launches the Node.js backend as a child process on a dynamically reserved port
- Polls `/api/health` until healthy (20s timeout)
- Exposes `desktop_bootstrap` Tauri command returning API origin + WS URL to the frontend
- Kills the backend child process on app exit

Desktop builds are orchestrated by `Launch-PixelDesktop.ps1` which handles CargoTools, sccache, staging, and build manifests. Always use the root npm scripts (`npm run desktop*`) rather than running cargo or tauri directly.

### Data Flow

1. Backend polls `adb devices -l` and stores results in SQLite registry
2. WebSocket broadcasts device + tool snapshots to all connected clients every 2 seconds
3. Frontend receives snapshots and re-renders; also makes REST calls for actions (tap, shell, scrcpy, approve)
4. Screenshots are captured via `adb exec-out screencap -p` and served as PNG blobs
5. Screen preview supports click-to-tap with coordinate mapping from display to device resolution

### Persistence

Local SQLite database at `data/device-registry.db` (WAL mode). Contains device approval state and session history. The `data/` directory is gitignored.

### Tooling

`vendor/` holds repo-local adb and scrcpy binaries, bootstrapped by `tools/bootstrap-tools.mjs`. A `vendor/tool-manifest.json` tracks resolved paths. The vendor directory is gitignored.

## Key Conventions

- Root npm scripts are the canonical entry points; avoid ad hoc workspace commands for desktop builds
- Server uses `node:sqlite` (Node 22+ built-in SQLite) - no external DB driver
- All device operations are keyed by serial number
- Environment variables: `HOST`, `PORT`, `ANDROID_CONTROL_DATA_ROOT`, `PIXEL_NODE_PATH`
- Node 24, TypeScript strict mode, ES2022 target
- Biome for linting/formatting (space indentation, recommended rules)
- CI runs on GitHub Actions: secret scan (Ubuntu), lint/typecheck/frontend tests (Ubuntu), desktop build (Windows)
