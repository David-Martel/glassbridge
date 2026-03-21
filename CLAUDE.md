# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

GlassBridge is a local-first Android device access framework for Windows desktop operators. It combines a Fastify control plane, a Vite browser UI, and a Tauri desktop shell around real `adb` and `scrcpy` workflows to manage multiple approved devices concurrently.

## Build and Development Commands

The recommended entry point is `just` (justfile) or `build.ps1` (PowerShell). Both
integrate CargoTools, sccache, biome autofix, and cargo-nextest automatically.

```bash
# Initial setup
just setup                 # npm ci + bootstrap tools + install hooks + cargo binstall acceleration tools

# Development
just dev                   # Server at :3030, Web at :5173 (hot reload)
just desktop-dev           # Tauri desktop shell (dev mode)

# Build
just build                 # Build server + web frontend
just build-all             # Build everything including desktop release binary
just desktop-build         # Desktop shell only (via Launch-PixelDesktop.ps1 + CargoTools)

# Lint and format
just lint                  # Check only (biome + cargo fmt --check)
just fix                   # Auto-fix all (biome --write + cargo fmt)
just check                 # Full static analysis (lint + typecheck + clippy)

# PowerShell alternative (build.ps1 — same capabilities)
pwsh -File build.ps1                   # Default: lint + typecheck + clippy
pwsh -File build.ps1 -Lint -Fix        # Auto-fix biome + cargo fmt
pwsh -File build.ps1 -Build            # Full build including desktop
pwsh -File build.ps1 -Test             # Deviceless tests (unit + Rust)
pwsh -File build.ps1 -Test -Nextest    # Use cargo-nextest for Rust tests
pwsh -File build.ps1 -Release          # Full release validation
pwsh -File build.ps1 -Clean            # Clean all build artifacts
```

### npm scripts (still available for direct use)
```bash
npm run lint               # Biome check
npm run lint:fix           # Biome autofix (--write + --unsafe)
npm run typecheck          # TypeScript --noEmit on both workspaces
npm run build              # Build both server and web workspaces
npm run desktop:build      # Desktop release build via Launch-PixelDesktop.ps1
```

## Testing

```bash
# Recommended (via just)
just test                  # Fast deviceless: unit + Rust (uses nextest)
just test-all              # Full suite: unit + Rust + device + integration
just test-device           # Device tests only (gracefully skips if no device)
just test-rust             # Rust tests via cargo-nextest
just test-integration      # Frontend/backend integration
just test-desktop          # Desktop build verification

# npm scripts
npm run test:unit          # vitest - DOM/UI tests (no device needed)
npm run test:device        # node --test - device smoke tests (skips gracefully)
npm run test:integration   # vitest - frontend/backend contract (device required)
npm run test:ci            # build + unit only (CI-safe, no device)
npm run test:all           # full suite with graceful device skipping
npm run test:rust          # cargo test on Tauri crate

# Single test file
npx vitest run tests/frontend-ui.test.ts
```

Device-dependent tests use `hasDevice()` from `tests/test-helpers.mjs` to detect a connected
Android device. Without a device, tests skip with exit code 0 (not fail). Set
`GLASSBRIDGE_REQUIRE_DEVICE=1` to hard-fail without a device (for CI with hardware).

Frontend tests use vitest with jsdom. Backend tests use Node's built-in test runner. Rust tests use cargo-nextest with profiles defined in `.config/nextest.toml` (default, ci, device).

## Validation and Security

```bash
# Recommended (via just)
just validate-commit       # Pre-commit gate (~30s): secrets + lint + typecheck + unit
just validate-push         # Pre-push gate (~3min): + build + integration + clippy + desktop check
just validate-release      # Full release: + secrets history + all tests + desktop build

# Secret scanning
just secrets-staged        # gitleaks on staged content
just secrets-repo          # gitleaks on current repo
just secrets-full          # gitleaks on repo + full history
```

Git hooks (`.githooks/`) run `validate:commit` on commit and `validate:push` on push. Install with `just hooks` or `npm run hooks:install`.

## Build Acceleration

- **sccache**: Rust compiler cache (30GB, zstd compression). Started automatically by `build.ps1` and `Launch-PixelDesktop.ps1`. Stats: `just cache-stats`.
- **cargo-nextest**: Parallel Rust test runner with profiles in `.config/nextest.toml`. Used by `just test-rust`.
- **cargo-binstall**: Pre-built binary installer for Rust tools. `just install-tools` installs nextest + sccache.
- **CargoTools**: PowerShell module for managed Rust environments. Detected and used automatically by `build.ps1` and `Launch-PixelDesktop.ps1` when available.

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
