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

## Phase 0: Foundation & CI/CD Hardening
*Priority: Immediate — unblocks all other phases*

### 0.1 Test Infrastructure Overhaul
- [ ] Add device detection helper to `tests/test-helpers.mjs` — export `hasDevice()` that runs
      `adb devices` and returns true if an authorized device is connected
- [ ] Modify `tests/device-smoke.test.mjs` — wrap each test in a `before()` guard that calls
      `hasDevice()` and skips with `test.skip()` when no device is present
- [ ] Modify `tests/frontend-backend.integration.test.ts` — same device-guard pattern
- [ ] Add `test:unit` script to `package.json` — runs only deviceless tests (vitest frontend)
- [ ] Add `test:device` script — runs device-dependent tests only
- [ ] Add `test:all` script — runs full suite (equivalent to current `test`)
- [ ] Update `test:ci` to use `test:unit` (no device dependency)

### 0.2 Git Hook Optimization
- [ ] Update `tools/pre-commit.ps1` — run `secrets:staged`, `lint`, `typecheck`, `test:unit`
      (drop `desktop:check` from pre-commit, keep in pre-push)
- [ ] Update `tools/pre-push.ps1` — run `secrets:repo`, `lint`, `typecheck`, `test:unit`,
      `desktop:check` (skip device tests — they hard-fail in CI and on machines without USB)
- [ ] Add `validate:full` script — the complete suite including device tests, for explicit use

### 0.3 GitHub Actions Redesign
- [ ] Refactor `.github/workflows/ci.yml`:
  - Job `lint-and-test`: Ubuntu, runs secret scan + lint + typecheck + `test:unit`
  - Job `rust-checks`: Ubuntu, runs `cargo fmt --check` + `cargo clippy` + `cargo test`
    on `apps/web/src-tauri/`
  - Job `desktop-build`: Windows, runs `desktop:check` + `desktop:build` + artifact upload
- [ ] Add `.github/workflows/device-test.yml`:
  - Trigger: `workflow_dispatch` + PR label `hardware-in-the-loop`
  - Runs on self-hosted runner with USB device access
  - Runs `test:device` + `test:integration` + Playwright headed tests
- [ ] Add Cargo caching to Windows desktop-build job (actions/cache for cargo registry + target)
- [ ] Add Rust toolchain component caching (clippy, rustfmt)

### 0.4 Remaining Dependabot Vulnerability
- [ ] Investigate moderate vulnerability reported on both David-Martel/pixel and
      David-Martel/glassbridge (check `gh api repos/David-Martel/pixel/dependabot/alerts`)
- [ ] Apply fix or suppress with documented rationale

---

## Phase 1: Native Rust HTTP Server (Replace Node.js Backend)
*Priority: High — eliminates Node.js child process spawn, ~1-2s startup savings*

### 1.1 Add Rust HTTP Dependencies
- [ ] Add to `apps/web/src-tauri/Cargo.toml`:
  ```toml
  axum = { version = "0.8", features = ["ws", "macros"] }
  axum-extra = { version = "0.10", features = ["typed-header"] }
  tower-http = { version = "0.6", features = ["cors", "fs"] }
  tokio = { version = "1", features = ["full"] }
  rusqlite = { version = "0.34", features = ["bundled"] }
  serde_json = "1"
  ```
- [ ] Pin `reqwest` to async-only (remove `blocking` feature, keep `rustls-tls`)

### 1.2 Create Rust Server Module
- [ ] Create `apps/web/src-tauri/src/server.rs`:
  - Axum router with all routes from `apps/server/src/index.ts`
  - `/api/health` — health check
  - `/api/devices` — device listing (calls adb module)
  - `/api/devices/:serial/screenshot` — PNG screenshot
  - `/api/devices/:serial/control` — key event injection
  - `/api/devices/:serial/tap` — tap injection
  - `/api/devices/:serial/text` — text input
  - `/api/devices/:serial/apps` — package listing
  - `/api/devices/:serial/apps/start` — launch package
  - `/api/devices/:serial/apps/stop` — force stop
  - `/api/devices/:serial/shell` — shell command
  - `/api/devices/:serial/scrcpy` — scrcpy launch
  - `/api/approved-devices` — approved device listing
  - `/api/approved-devices/:serial/approve` — approve device
  - `/api/tools/status` — tool status
  - `/api/tools/bootstrap` — tool bootstrap
  - `/api/help/topics` — help content
  - `/ws` — WebSocket snapshot broadcast (2s interval)

### 1.3 Create Rust Registry Module
- [ ] Create `apps/web/src-tauri/src/registry.rs`:
  - SQLite database via `rusqlite` (WAL mode)
  - `device_registry` table — same schema as `apps/server/src/registry.ts`
  - `device_sessions` table — session tracking
  - `sync_seen_devices()`, `approve_device()`, `list_approved()`, `get_registry_map()`
  - Connection pooling via `r2d2` or single connection with mutex

### 1.4 Create Rust ADB Module (Process-Based, Transitional)
- [ ] Create `apps/web/src-tauri/src/adb.rs`:
  - Port all functions from `apps/server/src/adb.ts` to Rust
  - Use `tokio::process::Command` instead of `spawn` (async, non-blocking)
  - `list_devices()` — parse `adb devices -l` output
  - `capture_screenshot()` — `adb exec-out screencap -p` with buffer collection
  - `send_key()`, `tap()`, `input_text()` — key/tap/text injection
  - `list_packages()`, `launch_package()`, `stop_package()` — app ops
  - `shell_command()` — generic shell execution
  - `start_scrcpy()` — detached scrcpy process launch
  - Tool path resolution from vendor manifest

### 1.5 Integrate Server into Tauri
- [ ] Modify `apps/web/src-tauri/src/main.rs`:
  - Remove `Command::new("node")` child process spawn
  - Start embedded Axum server on reserved port via `tokio::spawn`
  - Remove `wait_for_health()` polling — server is in-process, ready immediately
  - Keep `desktop_bootstrap` Tauri command returning port/URLs
- [ ] Update `apps/web/src-tauri/tauri.conf.json`:
  - Remove `beforeBuildCommand` (no longer need to build Node.js server first)
  - Keep `beforeDevCommand` for Vite dev server only
  - Update CSP if needed for new server port handling

### 1.6 Preserve Browser-Mode Compatibility
- [ ] Keep `apps/server/` as standalone Node.js server for browser-only development
- [ ] Add feature flag: `--features embedded-server` for Tauri desktop builds
- [ ] Browser mode (`npm run dev`) still runs Node.js server + Vite proxy
- [ ] Desktop mode uses embedded Rust server

### 1.7 Update Build Pipeline
- [ ] Modify `Launch-PixelDesktop.ps1`:
  - Remove `npm run build --workspace apps/server` step for desktop builds
  - Server is now compiled into the Tauri binary
  - Keep Vite frontend build (still needed for `frontendDist`)
- [ ] Update `npm run desktop:build` to skip server TypeScript compilation
- [ ] Verify `staging/desktop/glassbridge_desktop.exe` is self-contained

---

## Phase 2: Native ADB Protocol (Replace adb.exe Spawning)
*Priority: High — eliminates 200-500ms per-command spawn overhead*

### 2.1 Add Git Submodule for ADB Client
- [ ] **RECOMMENDED**: `adb_client` v3.1.2 (cocool97/adb_client, MIT, 341 stars, Mar 2026)
  - Full ADB protocol: device listing, shell, push/pull, framebuffer, mDNS discovery
  - Connection modes: USB (`nusb`), TCP (WiFi ADB), Server proxy
  - Alternatives evaluated:
    - `forensic-adb` v0.8.1 (kpcyrd, MPL-2.0, tokio-async, from Mozilla mozdevice)
    - `adb-client-tokio` v0.0.8 (devicelink, tokio-native async)
    - `mozdevice` (Mozilla production, ~1.4K downloads/month)
  - **Decision**: Use `adb_client` for broadest feature coverage + USB support
- [ ] Add to `Cargo.toml`: `adb_client = { version = "3.1", features = ["usb"] }`
- [ ] Add USB transport: `nusb = "0.2"` (pure Rust, async, no libusb, MIT+Apache2)

### 2.2 Implement Native ADB Transport
- [ ] Create `apps/web/src-tauri/src/adb_native.rs`:
  - USB transport via `nusb` crate (pure Rust, async, no libusb dependency)
  - TCP transport for WiFi ADB (wireless debugging over port 5555)
  - ADB protocol implementation: CONNECT, OPEN, WRITE, CLOSE, AUTH
  - Device authentication (RSA key pair management)
- [ ] Implement native device operations:
  - `native_list_devices()` — enumerate USB devices with ADB interface
  - `native_shell()` — open shell stream, execute command, collect output
  - `native_screencap()` — `screencap -p` via shell stream (avoid exec-out overhead)
  - `native_input_event()` — key, tap, text via `input` command
  - `native_list_packages()` — `pm list packages -3`
  - `native_push()` / `native_pull()` — file transfer (future)

### 2.3 Implement Native Screenshot Pipeline
- [ ] Direct framebuffer capture via ADB protocol (bypass screencap binary on device)
- [ ] PNG encoding in Rust (`image` crate or `png` crate — already a transitive dep)
- [ ] Optional: raw framebuffer → JPEG for faster transfer (lower quality, ~3x faster)
- [ ] Async streaming: start capture before previous frame is fully transferred

### 2.4 Switchable Backend
- [ ] Abstract device operations behind a Rust trait:
  ```rust
  #[async_trait]
  trait DeviceOps {
      async fn list_devices(&self) -> Result<Vec<Device>>;
      async fn screenshot(&self, serial: &str) -> Result<Vec<u8>>;
      async fn shell(&self, serial: &str, cmd: &str) -> Result<String>;
      // ...
  }
  ```
- [ ] `AdbProcessBackend` — existing adb.exe spawning (fallback)
- [ ] `AdbNativeBackend` — new native USB/TCP implementation
- [ ] Runtime selection: prefer native, fall back to process-based if native fails
- [ ] Feature flag: `--features native-adb` (default on for desktop builds)

### 2.5 WiFi ADB Support (New Feature)
- [ ] Implement TCP-based ADB transport for wireless debugging
- [ ] Device pairing workflow (QR code or PIN)
- [ ] Auto-discover devices on local network via mDNS
- [ ] Add UI controls for WiFi device management

---

## Phase 3: GPU-Accelerated Screen Mirroring (scrcpy Integration)
*Priority: Medium — replaces external scrcpy with embedded wgpu renderer*

### 3.1 Add scrcpy Source as Git Submodule
- [ ] `git submodule add https://github.com/Genymobile/scrcpy.git vendor/scrcpy-src`
- [ ] Pin to latest stable release tag
- [ ] Document build requirements (FFmpeg, SDL — needed only for reference builds)

### 3.2 Implement scrcpy Protocol in Rust
- [ ] Create `apps/web/src-tauri/src/scrcpy_protocol.rs`:
  - scrcpy server deployment: push `scrcpy-server.jar` to device via ADB
  - scrcpy tunnel setup: ADB forward/reverse TCP port
  - Video stream: H.264/H.265 raw NAL units over socket
  - Control stream: binary control messages (inject touch, key, scroll, etc.)
  - Device message stream: clipboard sync, device info responses

### 3.3 H.264 Decoding
- [ ] Add video decoding dependency (choose ONE):
  - **Option A (RECOMMENDED)**: `vk-video` — Vulkan Video H.264 decode, **zero-copy GPU**,
    frames never leave GPU memory. Works with wgpu. NVIDIA/AMD on Windows/Linux.
  - Option B: `ffmpeg-next` / `ffmpeg-sys-next` — full codec suite, NVDEC/DXVA2 hw accel,
    LGPL (dynamic linking preserves MIT). Most mature but heaviest dependency.
  - Option C: `openh264` v0.2.5 (Cisco, BSD-2) — CPU decode, constrained baseline only,
    no B-frames/CABAC/high profile. Simpler but limited. 3x faster with nasm.
  - Option D: `h264-decoder` — pure Rust, minimal docs, experimental
- [ ] Implement frame decoder: NAL unit parsing → YUV frame → GPU texture upload
- [ ] Reference: `hylarana` (mycrl/hylarana) — Rust H.264+Opus streaming with hw accel,
  Android virtual display support. Study for architecture patterns.

### 3.4 wgpu Rendering Pipeline
- [ ] Add wgpu dependencies:
  ```toml
  wgpu = "24"
  winit = "0.30"  # or use Tauri's window handle
  ```
- [ ] Create `apps/web/src-tauri/src/renderer.rs`:
  - wgpu surface from Tauri window handle (`raw-window-handle`)
  - Texture upload pipeline: decoded frame → GPU texture
  - Shader: simple fullscreen quad with YUV→RGB conversion (GPU-side)
  - Frame pacing: target 60fps, drop frames if decoder falls behind
  - Resize handling: maintain aspect ratio with letterboxing

### 3.5 Input Injection via Mirroring Window
- [ ] Map mouse/touch events on wgpu surface to device coordinates
- [ ] Send control messages through scrcpy control socket
- [ ] Keyboard passthrough for text input
- [ ] Gesture support: pinch-to-zoom, swipe, long-press

### 3.6 Tauri Integration
- [ ] Register wgpu renderer as a Tauri window or overlay
- [ ] Tauri command: `start_mirror(serial, preset)` → launches decoder + renderer
- [ ] Tauri command: `stop_mirror(serial)` → stops stream, releases GPU resources
- [ ] Multi-device: support concurrent mirrors in separate windows

---

## Phase 4: Additional Native Tools & Optimizations
*Priority: Medium-Low — incremental improvements after core migration*

### 4.1 Native Logcat Streaming
- [ ] Use `rogcat` v0.5.0 (flxo/rogcat) patterns for logcat parsing
- [ ] Implement `adb logcat` streaming via native ADB transport
- [ ] Filter by tag, priority, PID (rogcat-style colored output)
- [ ] Stream to UI via WebSocket or Tauri events
- [ ] Log buffer management (ring buffer, export)

### 4.2 File Transfer (Push/Pull)
- [ ] Native ADB SYNC protocol for file push/pull
- [ ] Drag-and-drop file transfer from desktop to device
- [ ] Progress indication for large files

### 4.3 Device Property Cache
- [ ] Cache `getprop` results per device session
- [ ] Prefetch common properties (model, SDK version, screen resolution)
- [ ] Invalidate on device reconnect

### 4.4 Multi-Device Optimization
- [ ] Parallel device operations (concurrent ADB connections)
- [ ] Device-specific settings persistence
- [ ] Fleet management UI improvements

### 4.5 Performance Monitoring
- [ ] Add telemetry for operation latencies
- [ ] Compare native vs process-based backends
- [ ] Dashboard for performance regression detection

---

## Phase 5: Agent-Bus Integration
*Priority: Low — enables multi-agent coordination for future development*

### 5.1 Agent-Bus MCP Client
- [ ] Add `agent-bus` as a development dependency (MCP client)
- [ ] Register GlassBridge as an agent on the bus during development sessions
- [ ] Enable agent-to-agent coordination for parallel development workflows

### 5.2 Device State Broadcasting
- [ ] Broadcast device connection/disconnection events to agent bus
- [ ] Enable external agents to query device state
- [ ] Support remote device operations via agent bus messages

---

## Git Submodule Strategy

### Planned Submodules
| Path | Repository | Purpose |
|------|-----------|---------|
| `vendor/scrcpy-src` | Genymobile/scrcpy | scrcpy protocol reference + server JAR |
| `vendor/rust/adb-client` | (TBD based on evaluation) | Native ADB protocol library |

### Submodule Guidelines
- Pin all submodules to release tags, not branches
- Update submodules in dedicated PRs with changelog notes
- CI must initialize submodules (`git submodule update --init --recursive`)

---

## Architecture Target State

```
┌─────────────────────────────────────────────────────────┐
│ Tauri Frontend (TypeScript/Vite)                       │
│ - Vanilla TS, same UI as today                         │
│ - WebSocket → embedded Axum server                     │
│ - wgpu mirror window (Phase 3)                         │
└─────────────────┬──────────────────────┬────────────────┘
                  │ HTTP/WS              │ wgpu surface
┌─────────────────▼──────────────────────▼────────────────┐
│ Embedded Rust Server (single binary)                   │
│                                                         │
│ ┌─────────┐ ┌──────────┐ ┌───────────┐ ┌────────────┐ │
│ │  Axum   │ │ Registry │ │ ADB Native│ │  scrcpy    │ │
│ │ Routes  │ │ (SQLite) │ │ (USB/TCP) │ │  Protocol  │ │
│ └────┬────┘ └────┬─────┘ └─────┬─────┘ └──────┬─────┘ │
│      │           │             │               │       │
│ ┌────▼───────────▼─────────────▼───────────────▼─────┐ │
│ │              Tokio Runtime                          │ │
│ └─────────────────────────────────────────────────────┘ │
│                                                         │
│ ┌─────────────┐  ┌────────────┐  ┌──────────────────┐ │
│ │ wgpu Render │  │ H.264      │  │ Device Property  │ │
│ │ Pipeline    │  │ Decoder    │  │ Cache            │ │
│ └─────────────┘  └────────────┘  └──────────────────┘ │
└─────────────────────────────────────────────────────────┘
         │ USB (nusb)          │ TCP (WiFi ADB)
┌────────▼────────────────────▼──────────────────────────┐
│ Android Device (Pixel 10 Pro XL)                       │
│ - ADB daemon                                            │
│ - scrcpy-server.jar (pushed on demand)                 │
└─────────────────────────────────────────────────────────┘
```

---

## Success Criteria

| Metric | Current | Phase 1 Target | Phase 2 Target | Phase 3 Target |
|--------|---------|----------------|----------------|----------------|
| Desktop cold start | 2-5s | <500ms | <500ms | <500ms |
| Device listing | 57ms | 57ms (still adb.exe) | <20ms | <20ms |
| Screenshot capture | 1,461ms | 1,461ms | <500ms | N/A (streaming) |
| Shell command | 123ms | 123ms | <50ms | <50ms |
| Screen mirror start | 2-3s (scrcpy) | 2-3s | 2-3s | <1s |
| Binary count | 3 (tauri+node+adb) | 2 (tauri+adb) | 1 (tauri only) | 1 |
| Process spawns/10s | 5-10 | 2-5 | 0 | 0 |
