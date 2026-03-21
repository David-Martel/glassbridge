# GlassBridge Native Rust Migration — Implementation Plan

> **Goal**: Replace the Node.js → adb.exe process-spawning architecture with a fully native
> Rust stack compiled into the Tauri desktop binary. Minimize external process overhead,
> maximize performance, and add GPU-accelerated screen mirroring via wgpu.
>
> **Reference device**: Pixel 10 Pro XL (serial: 58090DLCQ005ZZ, codename: mustang)
>
> **Baseline latencies** (current Node.js + adb.exe spawning):
> | Operation | Latency |
> |-----------|---------|
> | Device listing | 57ms |
> | Screenshot capture | 1,461ms |
> | Shell command | 123ms |
> | Package listing (271 pkgs) | 313ms |
> | Desktop cold start | ~2-5s |

---

## Phase 0: Foundation & CI/CD Hardening — COMPLETE
*Committed: a487a03*

- [x] Device detection helper (`hasDevice()` in test-helpers.mjs)
- [x] Graceful test skipping (device-smoke.test.mjs, integration test)
- [x] Test scripts: `test:unit`, `test:device`, `test:all`, `test:ci`
- [x] Pre-commit hook optimized (~30s, no device tests)
- [x] Pre-push hook: 9-step validation (no device tests)
- [x] `validate:full` script for explicit full suite
- [x] CI redesigned: lint-and-test + rust-checks + desktop-shell + device-test.yml
- [x] Dependabot: glib v0.18.5 assessed (Linux-only, blocked upstream, zero Windows impact)
- [x] `build.ps1` — CargoTools + sccache + biome autofix
- [x] `justfile` — 42 recipes with nextest/sccache/binstall acceleration
- [x] `nextest.toml` — default, ci, device profiles

---

## Phase 1: Native Rust HTTP Server — COMPLETE
*Committed: dc41eba*

- [x] Axum 0.8 + tower-http + tokio + rusqlite + serde_json dependencies
- [x] `server.rs` — all 17 REST/WS routes ported from Fastify (27 tests)
- [x] `registry.rs` — rusqlite SQLite WAL registry (20 tests)
- [x] `adb.rs` — DeviceOps trait + AdbProcessBackend (15 tests)
- [x] `tooling.rs` — vendor tool path resolution (7 tests)
- [x] `config.rs` — environment config (5 tests)
- [x] `help.rs` — static help content (5 tests)
- [x] `main.rs` rewritten: embedded Axum, no Node.js spawn, instant startup
- [x] Legacy TypeScript marked `@deprecated` (6 files)
- [x] Browser-mode preserved (`npm run dev` still works)

---

## Phase 2: Native ADB Protocol — CODE COMPLETE
*Committed: 577cba9*

- [x] `adb_client` v3.1 added to Cargo.toml (feature-gated: `native-adb`)
- [x] `adb_native.rs` — AdbNativeBackend via adb_client TCP (14 tests)
- [x] DeviceOps trait with async-trait for dyn compatibility
- [x] `create_adb_backend()` factory: native preferred, process fallback
- [x] `wifi_adb.rs` — connect/disconnect/pair/tcpip/discovery (22 tests)

### Remaining integration work:
- [ ] Wire `create_adb_backend()` into `server.rs` AppState::new() (currently uses AdbProcessBackend directly)
- [ ] Add Axum routes for WiFi ADB operations (connect, disconnect, pair)
- [ ] Add mDNS device discovery (`adb_client` mDNS feature)
- [ ] Benchmark native vs process backend with Pixel 10 Pro XL
- [ ] Update frontend UI with WiFi device management controls

---

## Phase 3: GPU-Accelerated Screen Mirroring — CODE COMPLETE
*Committed: 577cba9*

- [x] `scrcpy_protocol.rs` — full wire protocol: video + control + device messages (24 tests)
- [x] `decoder.rs` — openh264 H.264 decoder with YUV plane extraction (9 tests)
- [x] `renderer.rs` — wgpu 24 rendering pipeline, YUV→RGB GPU shader (10 tests)
- [x] Feature-gated: `--features screen-mirror` (default on)

### Remaining integration work:
- [ ] Wire scrcpy client into Axum server as Tauri commands (`start_mirror`, `stop_mirror`)
- [ ] Embed or locate scrcpy-server.jar for device deployment
- [ ] Create Tauri window for wgpu renderer (raw-window-handle surface)
- [ ] Implement input injection: mouse→touch, keyboard→key events
- [ ] Gesture support: pinch-to-zoom, swipe, long-press
- [ ] Frame pacing and dropped frame handling
- [ ] Multi-device concurrent mirror support
- [ ] Evaluate `vk-video` for zero-copy GPU decode (upgrade from openh264 CPU decode)

---

## Phase 4: Additional Native Tools — CODE COMPLETE
*Committed: 577cba9*

- [x] `logcat.rs` — streaming logcat with broadcast channel, filtering (22 tests)
- [x] `file_transfer.rs` — ADB push/pull with progress tracking (12 tests)
- [x] `property_cache.rs` — device property cache, 60s TTL (22 tests)
- [x] `telemetry.rs` — operation latency tracking with p95 (22 tests)

### Remaining integration work:
- [ ] Add Axum routes: `/api/devices/:serial/logcat` (WebSocket stream)
- [ ] Add Axum routes: `/api/devices/:serial/push`, `/api/devices/:serial/pull`
- [ ] Add Axum route: `/api/devices/:serial/properties` (cached)
- [ ] Add Axum route: `/api/telemetry` (performance dashboard data)
- [ ] Wire property cache into device listing (prefetch on connect)
- [ ] Wire telemetry into all device operations (RAII TimedOperation)
- [ ] Add logcat UI panel to frontend
- [ ] Add file transfer drag-and-drop UI
- [ ] Multi-device parallel operations (concurrent ADB connections)

---

## Phase 5: Agent-Bus Integration — CODE COMPLETE
*Committed: 577cba9*

- [x] `agent_bus.rs` — HTTP client for agent-bus coordination (13 tests)
- [x] Device event broadcasting (connect/disconnect/state_changed)
- [x] Silent no-op when bus unavailable

### Remaining integration work:
- [ ] Wire agent_bus into server startup (set_presence on launch)
- [ ] Broadcast device events from WebSocket snapshot loop
- [ ] Add `/api/agent-bus/status` route for bus health visibility

---

## Cross-Phase Integration TODO

### Build pipeline updates:
- [ ] Update `Launch-PixelDesktop.ps1` to skip Node.js server build for desktop
- [ ] Update `tauri.conf.json` `beforeBuildCommand` (frontend only, no server)
- [ ] Verify single-binary deployment (no node runtime needed)
- [ ] Add `--no-default-features` build option for minimal binary

### Frontend updates:
- [ ] Add logcat panel to UI
- [ ] Add file transfer UI (drag-and-drop)
- [ ] Add WiFi ADB device management controls
- [ ] Add performance telemetry dashboard
- [ ] Add scrcpy mirror controls (replace external scrcpy launch)

### Performance validation:
- [ ] Benchmark desktop cold start (target: <500ms)
- [ ] Benchmark native ADB device listing (target: <20ms)
- [ ] Benchmark native ADB screenshot (target: <500ms)
- [ ] Benchmark native ADB shell command (target: <50ms)
- [ ] Compare native vs process backend latencies
- [ ] Measure wgpu mirror startup time (target: <1s)

---

## Architecture — Current State

```
┌─────────────────────────────────────────────────────────┐
│ Tauri Frontend (TypeScript/Vite)                       │
│ - Vanilla TS, same UI as today                         │
│ - WebSocket → embedded Axum server                     │
│ - [TODO] wgpu mirror window                            │
│ - [TODO] logcat/file transfer/WiFi ADB UI              │
└─────────────────┬──────────────────────┬────────────────┘
                  │ HTTP/WS              │ wgpu surface
┌─────────────────▼──────────────────────▼────────────────┐
│ Embedded Rust Server (single binary) — 16 modules      │
│                                                         │
│ ┌─────────┐ ┌──────────┐ ┌───────────┐ ┌────────────┐ │
│ │  Axum   │ │ Registry │ │ ADB Native│ │  scrcpy    │ │
│ │ Routes  │ │ (SQLite) │ │ (USB/TCP) │ │  Protocol  │ │
│ │ [DONE]  │ │ [DONE]   │ │ [DONE]    │ │ [DONE]     │ │
│ └────┬────┘ └────┬─────┘ └─────┬─────┘ └──────┬─────┘ │
│      │           │             │               │       │
│ ┌────▼───────────▼─────────────▼───────────────▼─────┐ │
│ │              Tokio Runtime                          │ │
│ └─────────────────────────────────────────────────────┘ │
│                                                         │
│ ┌─────────────┐ ┌────────────┐ ┌──────────────────┐   │
│ │ wgpu Render │ │ H.264      │ │ Logcat/Transfer/ │   │
│ │ Pipeline    │ │ Decoder    │ │ Cache/Telemetry  │   │
│ │ [DONE]      │ │ [DONE]     │ │ [DONE]           │   │
│ └─────────────┘ └────────────┘ └──────────────────┘   │
│                                                         │
│ ┌──────────────┐ ┌────────────┐                        │
│ │ Agent Bus    │ │ WiFi ADB   │                        │
│ │ Client       │ │ [DONE]     │                        │
│ │ [DONE]       │ └────────────┘                        │
│ └──────────────┘                                        │
└─────────────────────────────────────────────────────────┘
         │ USB/TCP (adb_client)    │ TCP (WiFi ADB)
┌────────▼────────────────────────▼──────────────────────┐
│ Android Device (Pixel 10 Pro XL)                       │
│ - ADB daemon                                            │
│ - scrcpy-server.jar (pushed on demand)                 │
└─────────────────────────────────────────────────────────┘
```

---

## Test Coverage — 364 total (all passing with HIL)

| Suite | Tests | Device Required |
|-------|-------|-----------------|
| server.rs | 27 | No |
| registry.rs | 20 | No |
| adb.rs | 15 | No |
| adb_native.rs | 14 | No |
| scrcpy_protocol.rs | 24 | No |
| logcat.rs | 22 | No |
| property_cache.rs | 22 | No |
| telemetry.rs | 22 | No |
| wifi_adb.rs | 22 | No |
| file_transfer.rs | 12 | No |
| decoder.rs | 9 | No |
| renderer.rs | 10 | No |
| tooling.rs | 7 | No |
| help.rs | 5 | No |
| config.rs | 5 | No |
| adb_mock.rs | 5 | No |
| agent_bus.rs | 13 | No |
| lib.rs (desktop) | 3 | No |
| Frontend UI (vitest) | 1 | No |
| Device smoke (HIL) | 6 | Yes |
| Integration (HIL) | 4 | Yes |
| **Total** | **364** | |

---

## Success Criteria Progress

| Metric | Baseline | Target | Current Status |
|--------|----------|--------|----------------|
| Desktop cold start | 2-5s | <500ms | Code complete, needs benchmark |
| Device listing | 57ms | <20ms | Native backend ready, needs wiring |
| Screenshot capture | 1,461ms | <500ms | Native backend ready, needs wiring |
| Shell command | 123ms | <50ms | Native backend ready, needs wiring |
| Screen mirror start | 2-3s | <1s | Protocol + renderer ready, needs wiring |
| Binary count | 3 | 1 | 1 (embedded server), adb.exe still used as fallback |
| Process spawns/10s | 5-10 | 0 | Native backend ready, needs wiring |
| Rust tests | 0 | 353+ | 353 passing |
