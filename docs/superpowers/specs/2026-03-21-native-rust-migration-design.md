# GlassBridge Native Rust Migration — Design Specification

**Date**: 2026-03-21
**Status**: Draft
**Author**: Claude Code + David Martel

## Problem Statement

GlassBridge currently uses a triple-process architecture:
1. **Tauri binary** (Rust) spawns a **Node.js backend** (Fastify)
2. Node.js backend spawns **adb.exe** for every device operation
3. **scrcpy.exe** is launched as an unmonitored detached process

This creates:
- 2-5 second desktop cold start (Node.js spawn + health polling)
- 200-1,500ms latency per device operation (process spawn overhead)
- Three separate runtimes to maintain (Rust, Node.js, C/C++ binaries)
- No control over scrcpy lifecycle after launch

## Target Architecture

A single Tauri binary containing:
- **Axum HTTP/WebSocket server** (replacing Fastify)
- **Native ADB protocol** over USB/TCP (replacing adb.exe spawning)
- **scrcpy protocol client** with wgpu-accelerated H.264 rendering (replacing scrcpy.exe)
- **SQLite registry** via rusqlite (replacing node:sqlite)

## Design Decisions

### D1: Axum for HTTP Server
**Decision**: Use Axum (same framework as agent-bus) for the embedded HTTP server.
**Rationale**: Axum is the de facto standard for Rust async HTTP. The agent-bus project
already validates Axum + tokio + tower-http + WebSocket in this workspace. Shared patterns
reduce learning curve.
**Alternatives rejected**:
- `actix-web`: Different ecosystem, no workspace precedent
- Tauri IPC only: Would break browser-mode compatibility
- `warp`: Less maintained, smaller ecosystem

### D2: nusb for USB Transport
**Decision**: Use `nusb` crate for USB device communication.
**Rationale**: Pure Rust, async, no C library dependency (unlike `rusb` which needs libusb).
Windows-native via WinUSB. Actively maintained.
**Alternatives rejected**:
- `rusb` (libusb binding): Requires C library, harder to cross-compile
- `windows` crate raw WinUSB: Too low-level, platform-specific

### D3: Dual Backend (Native + Process Fallback)
**Decision**: Abstract device operations behind a trait with native and process-based
implementations. Default to native, fall back to process-based.
**Rationale**: Graceful degradation. If native USB fails (driver issues, permissions),
the existing adb.exe path still works. Enables incremental migration.

### D4: scrcpy Protocol Implementation (Not Fork)
**Decision**: Implement the scrcpy wire protocol in Rust rather than forking/patching
the scrcpy C codebase.
**Rationale**: The scrcpy protocol is well-documented and stable. The server JAR runs
on the Android device unchanged. Only the client-side (decoder + renderer) needs
reimplementation. This avoids maintaining a C/C++ build chain.

### D5: wgpu for Video Rendering
**Decision**: Use wgpu for GPU-accelerated frame rendering.
**Rationale**: wgpu provides cross-platform GPU access (Vulkan/DX12/Metal) with a
safe Rust API. YUV→RGB conversion runs as a GPU shader, freeing the CPU for decode.
Tauri windows provide `raw-window-handle` for wgpu surface creation.

### D6: FFmpeg for H.264 Decoding (Phase 3)
**Decision**: Use `ffmpeg-next` crate for H.264 decoding with hardware acceleration.
**Rationale**: FFmpeg supports NVDEC, DXVA2, and D3D11VA hardware decoders on Windows.
Pure-Rust decoders (openh264) lack hardware acceleration and would bottleneck at
1080p60. The FFmpeg shared library can be vendored alongside the Tauri binary.
**Future option**: If Media Foundation bindings improve, switch to Windows-native
decode to eliminate FFmpeg dependency entirely.

### D7: Git Submodules for External Source
**Decision**: Add scrcpy source as a git submodule at `vendor/scrcpy-src`.
**Rationale**: Provides protocol documentation, server JAR builds, and reference
client. Pinned to release tags for stability. The submodule is reference-only —
the Rust client is a clean-room implementation of the wire protocol.

### D8: Browser-Mode Preservation
**Decision**: Keep the Node.js server as a standalone development server.
**Rationale**: `npm run dev` provides fast iteration on frontend changes without
recompiling Rust. The Node.js server is the development-time backend; the Rust
server is the production/desktop backend. Feature parity between both.

## Component Design

### Rust Module Layout

```
apps/web/src-tauri/src/
├── main.rs              # Tauri app entry, server startup
├── server.rs            # Axum router, all REST/WS routes
├── registry.rs          # SQLite device registry (rusqlite)
├── adb.rs               # ADB operations trait + process-based impl
├── adb_native.rs        # Native ADB protocol (USB/TCP transport)
├── adb_protocol.rs      # ADB wire protocol: CONNECT, AUTH, OPEN, etc.
├── scrcpy_protocol.rs   # scrcpy server deploy + video/control streams
├── decoder.rs           # H.264 frame decoding (FFmpeg)
├── renderer.rs          # wgpu surface, texture upload, shaders
├── tooling.rs           # Vendor tool path resolution
├── help.rs              # Static help content
└── config.rs            # Environment + config management
```

### Data Flow (Phase 2+)

```
Frontend ──HTTP/WS──► Axum Router
                        │
                        ├──► RegistryDb (rusqlite) ──► data/device-registry.db
                        │
                        ├──► AdbNative ──USB(nusb)──► Android Device
                        │         │
                        │         └──TCP──► WiFi ADB Device
                        │
                        └──► ScrcpyClient
                               ├── Video Stream ──► H.264 Decoder ──► wgpu Renderer
                               └── Control Stream ◄── Mouse/Keyboard Events
```

### Error Handling

- All device operations return `Result<T, DeviceError>`
- `DeviceError` variants: `NotFound`, `Unauthorized`, `Offline`, `Timeout`, `Protocol`, `Io`
- HTTP routes map `DeviceError` to appropriate status codes (404, 403, 408, 500)
- WebSocket broadcast silently drops errors (same as current behavior)

### Testing Strategy

| Layer | Framework | Device Required |
|-------|-----------|-----------------|
| Rust unit tests | `cargo test` | No |
| Rust integration (mock ADB) | `cargo test --features mock-adb` | No |
| Rust integration (real device) | `cargo test --features device-test` | Yes |
| Frontend unit tests | vitest + jsdom | No |
| Frontend integration | vitest + real server | Optional |
| E2E | Playwright | Yes |
| Desktop build | cargo build --release | No |

## Phasing

| Phase | Scope | Effort | Impact |
|-------|-------|--------|--------|
| 0 | CI/CD + test infrastructure | 1-2 days | Unblocks everything |
| 1 | Embedded Axum server | 3-5 days | Eliminates Node.js spawn |
| 2 | Native ADB protocol | 5-8 days | Eliminates adb.exe spawning |
| 3 | wgpu screen mirroring | 8-12 days | Eliminates scrcpy.exe |
| 4 | Additional tools | Ongoing | Incremental improvements |
| 5 | Agent-bus integration | 1-2 days | Development workflow |

## Risks

| Risk | Mitigation |
|------|-----------|
| ADB protocol complexity | Fall back to process-based backend |
| USB driver issues on Windows | WinUSB via nusb is well-tested; adb.exe fallback |
| FFmpeg licensing (LGPL) | Dynamic linking preserves MIT license for GlassBridge |
| wgpu compatibility on older GPUs | Software fallback renderer |
| scrcpy protocol changes | Submodule pinned to stable release; protocol is versioned |
