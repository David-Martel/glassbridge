# GlassBridge Context - 2026-03-21

## Project Summary

GlassBridge is a local-first Android device access framework for Windows desktop operators. It combines a Fastify control plane (`apps/server`), a Vite browser UI (`apps/web`), and a Tauri v2 desktop shell (`apps/web/src-tauri`) around real `adb` and `scrcpy` workflows. npm workspace monorepo, TypeScript strict, Node 24.

## Current State

- **Branch:** `main` @ `28572d1`
- **Status:** Clean, fully synchronized with both remotes
- **Remotes:** `origin` (David-Martel/pixel, private) and `public` (David-Martel/glassbridge, public)
- All PRs closed/merged, no open branches, no worktrees

## Session Work (2026-03-21)

### Actions Taken
1. Created `CLAUDE.md` for Claude Code onboarding (architecture, commands, conventions)
2. Merged Dependabot PRs on both repos (rustls-webpki 0.103.9 -> 0.103.10)
3. Merged `release/glassbridge-public` branch into `main`
4. Rebased local main onto public/main to reconcile divergent dependabot commits
5. Pushed synchronized main to both origin and public
6. Closed glassbridge PR #1 (release prep)
7. Deleted `release/glassbridge-public` branch locally and on public remote

### Decisions Made

| Decision | Rationale |
|----------|-----------|
| Rebase onto public/main rather than merge | The two remotes had different dependabot merge commits (different SHAs for same dep bump). Rebasing produced a clean linear history on public's dependabot merge. |
| force-with-lease to origin after rebase | Origin had the old merge commit; after rebasing onto public's history, origin needed a force update. Used `--force-with-lease` for safety. |
| Close release PR rather than merge via GH | The release branch content was already merged locally into main and pushed; the PR was redundant. |

## Architecture Quick Reference

```
apps/server/src/
  index.ts      - Fastify routes, WS broadcast (2s interval)
  adb.ts        - All adb operations (device list, screenshot, tap, shell, scrcpy)
  registry.ts   - SQLite device registry (node:sqlite DatabaseSync)
  tooling.ts    - Tool path resolution, runCommand/runCommandBuffer
  config.ts     - Env-driven config (HOST, PORT, ANDROID_CONTROL_DATA_ROOT)
  help.ts       - Static operator help content

apps/web/src/
  main.ts       - Full UI (innerHTML-based rendering, state management, event binding)
  runtime.ts    - Runtime detection (browser vs Tauri desktop)
  styles.css    - All styles

apps/web/src-tauri/
  src/main.rs   - Tauri app: launches Node backend, reserves port, health polls
  Cargo.toml    - glassbridge_desktop crate (tauri 2, reqwest, serde)
  tauri.conf.json - Window config, CSP, build commands
```

## Key Technical Details

- **Server:** Fastify 5, Zod validation, ESM, `node:sqlite` (Node 22+ built-in)
- **Frontend:** Vanilla TypeScript, no framework, innerHTML + bindEvents pattern
- **Desktop:** Tauri v2, Rust, spawns Node backend as child process
- **Testing:** vitest (frontend/integration, jsdom), node:test (backend, device-required)
- **Linting:** Biome 2.x (recommended rules, space indent)
- **CI:** GitHub Actions - gitleaks, lint/typecheck/frontend (Ubuntu), desktop build (Windows)
- **Hooks:** Pre-commit runs `validate:staged`, pre-push runs `validate:release`
- **Data:** SQLite WAL at `data/device-registry.db` (gitignored)
- **Tooling:** adb/scrcpy bootstrapped into `vendor/` (gitignored)

## Known Issues

- Backend/integration tests require a physical Android device connected via USB
- Both repos show 1 moderate Dependabot vulnerability (separate from the one fixed today)
- Pre-push hook runs full `validate:release` including device tests - these fail without hardware but the hook still allows the push

## Recommended Next Actions

1. Investigate the remaining moderate Dependabot vulnerability on both repos
2. Consider making backend/integration tests skip gracefully when no device is connected (currently they hard-fail)
3. The pre-push hook could be optimized to skip device-dependent tests or run `test:ci` instead of full `test`
