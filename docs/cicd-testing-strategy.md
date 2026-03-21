# GlassBridge CI/CD and Testing Strategy

## Overview

This document describes the complete testing and CI/CD strategy for GlassBridge.
The core design principle is that **no test fails because hardware is absent** —
device-dependent tests skip cleanly, and every PR can be green without a Pixel
connected to a runner.

---

## 1. Test Categories

### 1.1 Unit Tests (`test:unit`)
- **Runner**: vitest, jsdom environment
- **File**: `tests/frontend-ui.test.ts`
- **Dependencies**: none (no server, no ADB, no device)
- **Gate**: pre-commit, CI (Ubuntu + Windows), PR merge requirement
- **What it covers**: Frontend render logic, fetch mock interactions, approval
  flow, screenshot blob handling

### 1.2 Integration Tests (`test:integration`)
- **Runner**: vitest, jsdom + real Node.js backend process
- **File**: `tests/frontend-backend.integration.test.ts`
- **Dependencies**: Node.js backend (`apps/server/dist/index.js`), ADB binary
  (optional)
- **Device required**: No — the backend starts cleanly with no device connected;
  the integration test verifies the frontend handles an empty device list
  correctly
- **Gate**: pre-push, CI (Ubuntu + Windows)
- **What it covers**: Real HTTP payloads from the live backend, frontend render
  against those payloads, graceful empty-state UI

### 1.3 Device Tests (`test:device`)
- **Runner**: `node --test` (Node built-in test runner)
- **File**: `tests/device-smoke.test.mjs`
- **Dependencies**: Real Android device, ADB, scrcpy, built server
- **Device required**: Yes, but gracefully skips when absent (see §2)
- **Gate**: `device-test.yml` workflow only (manual or `hardware-in-the-loop` PR label)
- **Reference device**: Pixel 10 XL (`ANDROID_TARGET_MODEL=Pixel 10`)
- **What it covers**: ADB tool presence, device enumeration, shell commands,
  package listing, device approval, screenshot capture, control actions

### 1.4 Desktop Tests (`test:desktop`)
- **Runner**: PowerShell launcher + Cargo
- **Files**: `apps/web/src-tauri/`, `Launch-PixelDesktop.ps1`
- **Dependencies**: Rust toolchain, MSVC linker (Windows), built Node.js server
- **Gate**: pre-push (via `desktop:check`), CI `desktop` job, release workflow
- **What it covers**: Rust fmt, clippy, unit tests in `lib.rs`/`adb_mock.rs`,
  successful `cargo build` of the Tauri binary

### 1.5 Rust Unit Tests (`test:rust`)
- **Runner**: `cargo test`
- **Files**: `apps/web/src-tauri/src/lib.rs`, `apps/web/src-tauri/src/adb_mock.rs`
- **Dependencies**: Rust toolchain only
- **Device required**: No
- **Gate**: CI `desktop` job, pre-push Rust check
- **What it covers**: `reserve_port`, `wait_for_health`, ADB output line parser,
  PNG fixture validation, mock ADB response fixtures

### 1.6 E2E Tests (`test:e2e`)
- **Runner**: Playwright
- **File**: `tests/playwright-live.mjs` (headless), `tests/playwright-live-headed.mjs` (local)
- **Dependencies**: Real device, built frontend + backend
- **Gate**: `device-test.yml` `device-e2e` job only
- **What it covers**: Full browser-level interaction with a live connected device

---

## 2. Graceful Device Detection

### Problem
The original `device-smoke.test.mjs` called `assert.ok(devices.length > 0, …)`
in `test.before()` which threw an uncaught exception, causing the entire test
process to exit with code 1 — indistinguishable from a real test failure.

### Solution: `tests/device-context.mjs`

A shared helper that encapsulates all device-detection logic:

```
resolveDeviceContext(baseUrl, readLogs)
  → { skipped: false, device, allDevices }   // device ready
  → { skipped: true,  reason, allDevices }   // no device — caller should skip
```

Detection sequence:
1. Probe `/api/tools/status` — skip if ADB or scrcpy missing
2. Probe `/api/devices` — skip if no authorised devices
3. Prefer `ANDROID_TARGET_MODEL` match; fall back to `devices[0]`

Each device-dependent test calls `skipWithReason(t, ctx.reason)` which emits a
structured skip diagnostic via `node:test`'s `t.skip()`.  The process exits
with code 0.

### Hard-Fail Mode
Set `GLASSBRIDGE_REQUIRE_DEVICE=1` to convert all skips to hard errors.  The
`test:device:ci` script sets this automatically for use on the self-hosted
runner where a device is guaranteed.

---

## 3. GitHub Actions Workflows

### 3.1 `ci.yml` — Deviceless Gate (every push/PR)

```
secret-scan
    └── validate          (Ubuntu: lint, typecheck, build, test:unit, test:integration)
    └── validate-windows  (Windows: same as above)
    └── desktop           (Windows: rust fmt, clippy, cargo test, desktop:check)
```

Caching strategy:
- **npm**: `actions/setup-node` built-in cache (`cache: npm`)
- **sccache store**: keyed on `Cargo.lock` hash; restores from a prefix key on
  miss so the first Cargo build after a dep change still gets a partial cache
- **Cargo registry + git db**: separate cache from the build output; these
  change infrequently and are large

The `desktop` job uses `RUSTC_WRAPPER: ""` for `cargo fmt` (no compilation)
and `RUSTC_WRAPPER: sccache` everywhere Rust is compiled.

### 3.2 `device-test.yml` — Hardware-in-the-Loop

Triggered by:
- `workflow_dispatch` (manual, with optional `target_model` input)
- PR `labeled`/`synchronize`/`reopened` events when the `hardware-in-the-loop`
  label is present

Jobs:
- **device-smoke**: Runs `test:device:ci` on a `[self-hosted, android-device]`
  runner.  Pre-checks `adb devices` before starting the test suite.
- **device-e2e**: Runs Playwright E2E with a real device.

Both jobs set `GLASSBRIDGE_REQUIRE_DEVICE=1` — they run on dedicated hardware
and must find a device.

### 3.3 `release-desktop.yml` — Tagged Release

Triggered by `v*` tags or `workflow_dispatch`.  Runs quality gates (fmt, clippy,
lint, typecheck) before the release build so a bad tag cannot produce a release
artifact.  Attaches the binary to the GitHub Release via
`softprops/action-gh-release`.

---

## 4. Git Hooks

### 4.1 Pre-Commit (`validate:commit`)
Target: < 30 seconds on a warm machine.

```
secrets:staged    secret scan on staged files only
lint              biome check
typecheck         tsc --noEmit across all workspaces
test:unit         vitest (frontend-ui.test.ts, jsdom, no server)
```

Device tests, integration tests, and Rust checks are excluded.  They are too
slow for commit-level feedback.

### 4.2 Pre-Push (`validate:push`)
Target: < 3 minutes on a warm machine with sccache.

```
secrets:repo      full repo scan
secrets:history   git log scan
lint
typecheck
build             full server + web build
test:unit
test:integration  real backend, no device
rust:fmt:check    cargo fmt --check
rust:check        cargo clippy -D warnings
desktop:check     debug Tauri build
```

Device tests are not run.  Use `npm run test:device` explicitly.

### 4.3 Full Validation (`validate:full` / `validate:release`)
Explicit command — not tied to any hook.

```
validate:push (all of the above)
test:device           with graceful skip if no device
test:desktop          full release Tauri build
```

---

## 5. Script Reference (`package.json`)

| Script | What it runs | Device required |
|--------|-------------|-----------------|
| `test:unit` | vitest frontend-ui.test.ts | No |
| `test:integration` | build server + vitest integration | No |
| `test:device` | build server + node:test device-smoke | Skip if absent |
| `test:device:ci` | same + GLASSBRIDGE_REQUIRE_DEVICE=1 | Hard-fail |
| `test:desktop` | build server + desktop:check + desktop:build | No |
| `test:rust` | cargo test (lib.rs + adb_mock.rs) | No |
| `test:e2e` | build + playwright headless | Yes |
| `test:e2e:headed` | build + playwright headed | Yes |
| `validate:commit` | secrets(staged) + lint + typecheck + test:unit | No |
| `validate:push` | secrets + lint + typecheck + build + test:unit + test:integration + rust:fmt:check + rust:check + desktop:check | No |
| `validate:full` | validate:push + test:device + test:desktop | Skip if absent |
| `validate:release` | alias for validate:full | Skip if absent |
| `rust:check` | cargo clippy -D warnings | No |
| `rust:fmt` | cargo fmt | No |
| `rust:fmt:check` | cargo fmt --check | No |
| `rust:test` | cargo test --all-features | No |

---

## 6. Rust Test Configuration

### Lib Target (`apps/web/src-tauri/src/lib.rs`)
Exposes `desktop::reserve_port` and `desktop::wait_for_health` as testable
public functions.  Unit tests confirm:
- `reserve_port` returns a bindable port
- `wait_for_health` times out correctly on unreachable endpoints

### Mock Fixtures (`apps/web/src-tauri/src/adb_mock.rs`)
Provides `const` string fixtures for `adb devices -l` output covering:
- One authorised Pixel 10 XL
- Empty device list
- One unauthorised device
- Two authorised devices

Tests verify the ADB line parser against each fixture without any ADB process.

### Cargo.toml Changes
- Added `[lib]` target so `cargo test` reaches the library without the binary
  entry point
- Added `[profile.test]` with `opt-level = 0` + `debug = true` for fast test
  compilation
- The `[[bin]]` target is kept separate to avoid the Tauri
  `windows_subsystem = "windows"` attribute interfering with tests

### nextest Profiles (`.config/nextest.toml`)
- `default`: 1 retry with 2 s delay (dev machines, port races)
- `ci`: no retries, fail-fast off (fail clearly, fix the root cause)
- `device`: fail-fast on (device may be in a bad state after first failure)

Run with: `cargo nextest run --profile ci`

---

## 7. Self-Hosted Runner Setup

The `android-device` runner label identifies machines with USB-attached Android
devices.  Required setup on each runner machine:

```bash
# 1. Register with GitHub Actions
./config.sh --url https://github.com/OWNER/pixel --token TOKEN --labels "android-device"

# 2. Install ADB and scrcpy (or use the bootstrap script)
npm run bootstrap:tools

# 3. Authorise the device
adb devices
# Accept the USB debugging prompt on the Pixel

# 4. Set environment variable
echo 'ANDROID_TARGET_MODEL=Pixel 10' >> ~/.profile

# 5. Verify
adb devices -l   # Should show device state (not unauthorized)
```

---

## 8. Adding New Device-Dependent Tests

1. Import the shared helper:
   ```js
   import { resolveDeviceContext, skipWithReason } from "./device-context.mjs";
   ```

2. Resolve context after the server is healthy:
   ```js
   const deviceCtx = await resolveDeviceContext(baseUrl, () => serverLogs);
   ```

3. Guard each test:
   ```js
   test("my device test", async (t) => {
     if (deviceCtx.skipped) return skipWithReason(t, deviceCtx.reason);
     const { device } = deviceCtx;
     // ... assertions using device.serial
   });
   ```

4. Place the test file alongside `device-smoke.test.mjs` and add it to the
   `test:device` script in `package.json`.

---

## 9. Future Work

- **Rust native backend**: When the Rust ADB layer replaces the TypeScript one,
  add `adb_mock.rs` integration tests that inject the mock command output into
  the parser under test.  The `adb-mock` feature flag pattern below gates them:

  ```rust
  #[cfg(feature = "adb-mock")]
  pub fn list_devices_from_str(output: &str) -> Vec<DeviceRecord> { … }

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::adb_mock::DEVICES_RESPONSE_ONE_PIXEL;

      #[test]
      fn parses_pixel_from_mock() {
          let devices = list_devices_from_str(DEVICES_RESPONSE_ONE_PIXEL);
          assert_eq!(devices.len(), 1);
      }
  }
  ```

- **Screenshot golden files**: Store reference PNGs under `tests/fixtures/` and
  compare structural properties (dimensions, format) rather than pixel-exact
  equality, to avoid churn across OS rendering differences.

- **ADB over TCP/IP**: When Wi-Fi ADB is supported, the device runner can
  connect wirelessly, removing the USB-proximity requirement for the self-hosted
  runner.
