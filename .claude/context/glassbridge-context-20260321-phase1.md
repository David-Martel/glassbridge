# GlassBridge Context — Phase 1 Native Rust Migration

## Project State

- **Branch:** `main` @ `a487a03` (Phase 0 committed, Phase 1 uncommitted)
- **Status:** Phase 1 validated, ready to commit
- **Validation:** lint clean, typecheck clean, clippy 0 warnings, 170/170 tests pass

## Session Summary

Major architectural migration from Node.js process-spawning to embedded native Rust server.
Orchestrated via agent-bus (`glassbridge-migration` group channel) with 12+ specialist agents
working in parallel across research, implementation, review, and fix tasks.

## Phase 0 (Committed: a487a03)

- Test infrastructure: device detection, graceful skip, `GLASSBRIDGE_REQUIRE_DEVICE` env
- CI/CD: 4 GitHub Actions jobs, hardware-in-the-loop workflow, optimized hooks
- Build tooling: `build.ps1` (CargoTools/sccache), `justfile` (42 recipes, nextest)
- Design spec + TODO.md with 5-phase migration plan

## Phase 1 (Validated, Uncommitted)

### New Rust Modules (apps/web/src-tauri/src/)

| Module | Size | Tests | Replaces |
|--------|------|-------|----------|
| `server.rs` | 40KB | 27 | apps/server/src/index.ts (Fastify) |
| `registry.rs` | 34KB | 20 | apps/server/src/registry.ts (node:sqlite) |
| `adb.rs` | 24KB | 15 | apps/server/src/adb.ts |
| `tooling.rs` | 14KB | 7 | apps/server/src/tooling.ts |
| `config.rs` | 4KB | 5 | apps/server/src/config.ts |
| `help.rs` | 4KB | 5 | apps/server/src/help.ts |

### main.rs Rewritten
- Embedded Axum server (no Node.js child process)
- Instant startup (no 20s health polling)
- Single binary deployment

### Dependencies Added
- `axum` 0.8 (HTTP/WS server)
- `rusqlite` 0.34 bundled (SQLite)
- `tower-http` 0.6 (CORS, static files)
- `async-trait` 0.1 (dyn-compatible DeviceOps)
- `tokio` 1.x full (async runtime)
- `tracing` 0.1 + subscriber (logging)
- `thiserror` 2 (error types)

### Legacy TypeScript Deprecated
All 6 files in `apps/server/src/` marked with `@deprecated` comment.
Retained for browser-mode development (`npm run dev`).

## Agent Work Registry

| Agent | Task | Files | Status |
|-------|------|-------|--------|
| research-agent | Crate survey (ADB/scrcpy/wgpu) | - | Complete |
| cicd-agent | CI/CD design | docs/cicd-testing-strategy.md | Complete |
| scrcpy-research-agent | scrcpy architecture analysis | - | Complete |
| test-infra-agent | Device detection + graceful skip | tests/*.mjs, tests/*.ts | Complete |
| cicd-impl-agent | Workflows + hooks | .github/, tools/ | Complete |
| security-agent | Dependabot + cargo update | Cargo.lock, Cargo.toml | Complete |
| code-reviewer | Phase 0 audit (14 findings) | - | Complete |
| rust-server-agent | Axum server module | server.rs, help.rs, config.rs | Complete |
| rust-registry-agent | SQLite registry | registry.rs | Complete |
| rust-adb-agent | ADB wrapper + tooling | adb.rs, tooling.rs | Complete |
| rust-fixer-agent | Compilation fixes | adb.rs, server.rs, registry.rs, Cargo.toml | Complete |

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| HTTP framework | Axum 0.8 | Same as agent-bus, workspace precedent |
| SQLite driver | rusqlite (bundled) | No external lib, WAL mode |
| Trait compatibility | async-trait crate | DeviceOps as dyn trait object in AppState |
| Route syntax | `{serial}` (axum 0.8) | Not `:serial` (express-style) |
| ADB client (Phase 2) | adb_client v3.1.2 | Full protocol, USB+TCP, 341 stars |
| H.264 decode (Phase 3) | vk-video | Zero-copy GPU via Vulkan Video |
| scrcpy integration | Wire protocol reimpl | No native Rust scrcpy exists |

## Baseline Performance

| Operation | Before (Node.js) | After (Rust) Target |
|-----------|------------------|---------------------|
| Desktop cold start | 2-5s | <500ms |
| Device listing | 57ms | 57ms (Phase 1), <20ms (Phase 2) |
| Screenshot | 1,461ms | 1,461ms (Phase 1), <500ms (Phase 2) |
| Binary count | 2 (tauri+node) | 1 (tauri only) |

## Next Steps (TODO.md)

1. **Phase 2**: Native ADB protocol (adb_client v3.1.2 + nusb v0.2.3)
2. **Phase 3**: wgpu screen mirroring (vk-video + scrcpy protocol)
3. **Phase 4**: Logcat streaming, file transfer, device property cache
4. **Phase 5**: Agent-bus integration for development coordination
