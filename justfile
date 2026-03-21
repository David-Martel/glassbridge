# GlassBridge — build, test, and development recipes
#
# Prerequisites:
#   just     — https://github.com/casey/just (cargo binstall just)
#   sccache  — https://github.com/mozilla/sccache (cargo binstall sccache)
#   nextest  — https://nexte.st (cargo binstall cargo-nextest)
#   binstall — https://github.com/cargo-bins/cargo-binstall
#   biome    — installed via npm (devDependency)

set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

repo_root    := justfile_directory()
tauri_manifest := repo_root / "apps" / "web" / "src-tauri" / "Cargo.toml"
nextest_cfg  := repo_root / ".config" / "nextest.toml"

# ── defaults ─────────────────────────────────────────────────────────────────

# Show available recipes
default:
    @just --list --unsorted

# ── setup ────────────────────────────────────────────────────────────────────

# Install all dependencies (npm + Rust tooling)
setup:
    npm ci
    npm run bootstrap:tools
    npm run hooks:install
    just install-tools

# Install Rust acceleration tools via cargo-binstall
install-tools:
    @echo "Installing build acceleration tools..."
    cargo binstall -y cargo-nextest sccache 2>$null; if ($LASTEXITCODE -ne 0) { Write-Warning "cargo-binstall not found or install failed — install tools manually" }

# ── lint & format ────────────────────────────────────────────────────────────

# Check formatting and lint (no changes)
lint:
    npx biome check .
    cargo fmt --manifest-path "{{tauri_manifest}}" --all --check

# Auto-fix formatting and lint issues
fix:
    npx biome check --write .
    npx biome check --write --unsafe .
    cargo fmt --manifest-path "{{tauri_manifest}}" --all

# Biome autofix only (TypeScript/JSON)
fix-ts:
    npx biome check --write .
    npx biome check --write --unsafe .

# Cargo fmt only (Rust)
fix-rs:
    cargo fmt --manifest-path "{{tauri_manifest}}" --all

# ── typecheck ────────────────────────────────────────────────────────────────

# TypeScript typecheck (both workspaces)
typecheck:
    npm run typecheck

# ── check (lint + typecheck + clippy, no build) ─────────────────────────────

# Full static analysis pass (lint + typecheck + clippy)
check: lint typecheck clippy

# Cargo clippy with deny warnings
clippy:
    cargo clippy --manifest-path "{{tauri_manifest}}" --all-targets -- -D warnings

# ── build ────────────────────────────────────────────────────────────────────

# Build server and web frontend
build:
    npm run build --workspace apps/server
    npm run build --workspace apps/web

# Build everything including desktop shell (release)
build-all: build desktop-build

# Desktop shell only (delegates to Launch-PixelDesktop.ps1 with CargoTools)
desktop-build:
    pwsh -ExecutionPolicy Bypass -File "{{repo_root}}/Launch-PixelDesktop.ps1" -BuildOnly

# Desktop check only (cargo check, no release build)
desktop-check:
    pwsh -ExecutionPolicy Bypass -File "{{repo_root}}/Launch-PixelDesktop.ps1" -CheckOnly

# Full build via build.ps1 (CargoTools + sccache)
build-ps:
    pwsh -ExecutionPolicy Bypass -File "{{repo_root}}/build.ps1" -Build

# ── test ─────────────────────────────────────────────────────────────────────

# Fast deviceless tests (frontend unit + Rust)
test: test-unit test-rust

# Frontend unit tests (vitest, jsdom)
test-unit:
    npx vitest run tests/frontend-ui.test.ts

# Rust tests with cargo-nextest (faster parallelism + better output)
test-rust:
    cargo nextest run --manifest-path "{{tauri_manifest}}" --all-features --config-file "{{nextest_cfg}}"

# Rust tests with plain cargo test (fallback if nextest unavailable)
test-rust-cargo:
    cargo test --manifest-path "{{tauri_manifest}}" --all-features

# Rust tests CI profile (no retries, fail output deferred)
test-rust-ci:
    cargo nextest run --manifest-path "{{tauri_manifest}}" --all-features --config-file "{{nextest_cfg}}" --profile ci

# Device smoke tests (requires connected Android device; skips gracefully)
test-device:
    npm run build --workspace apps/server
    node --test tests/device-smoke.test.mjs

# Device tests that hard-fail without device (for CI with hardware)
test-device-ci:
    npm run build --workspace apps/server
    $env:GLASSBRIDGE_REQUIRE_DEVICE = "1"; node --test tests/device-smoke.test.mjs

# Integration tests (frontend + real backend)
test-integration:
    npm run build --workspace apps/server
    npx vitest run tests/frontend-backend.integration.test.ts

# Desktop build verification (cargo check + release build)
test-desktop:
    npm run build --workspace apps/server
    just desktop-check
    just desktop-build

# Full test suite (all tests, device tests skip without hardware)
test-all: build test-unit test-rust test-device test-integration

# Rust nextest with device profile (fail-fast, no retries)
test-rust-device:
    cargo nextest run --manifest-path "{{tauri_manifest}}" --all-features --config-file "{{nextest_cfg}}" --profile device

# ── validation (pre-commit / pre-push gates) ────────────────────────────────

# Pre-commit validation (fast: ~30s)
validate-commit:
    npm run secrets:staged
    just lint
    just typecheck
    just test-unit

# Pre-push validation (medium: ~3min)
validate-push:
    npm run secrets:repo
    just lint
    just typecheck
    just build
    just test-unit
    just test-rust
    just test-integration
    just desktop-check

# Full release validation (everything)
validate-release:
    npm run secrets:repo
    npm run secrets:history
    just fix
    just typecheck
    just clippy
    just test-all
    just test-desktop

# ── secrets ──────────────────────────────────────────────────────────────────

# Scan staged content for secrets
secrets-staged:
    npm run secrets:staged

# Scan repo contents for secrets
secrets-repo:
    npm run secrets:repo

# Scan full git history for secrets
secrets-history:
    npm run secrets:history

# Full secret scan (repo + history)
secrets-full: secrets-repo secrets-history

# ── development ──────────────────────────────────────────────────────────────

# Start dev servers (Fastify + Vite, hot reload)
dev:
    npm run dev

# Start desktop shell (Tauri dev mode)
desktop-dev:
    pwsh -ExecutionPolicy Bypass -File "{{repo_root}}/Launch-PixelDesktop.ps1"

# Watch Rust for check errors on save
watch-rust:
    cargo watch -w "{{repo_root}}/apps/web/src-tauri/src" -x "clippy --manifest-path {{tauri_manifest}} --all-targets -- -D warnings"

# ── clean ────────────────────────────────────────────────────────────────────

# Clean all build artifacts
clean:
    pwsh -ExecutionPolicy Bypass -File "{{repo_root}}/build.ps1" -Clean

# Clean Rust target only
clean-rust:
    cargo clean --manifest-path "{{tauri_manifest}}"

# ── sccache ──────────────────────────────────────────────────────────────────

# Show sccache statistics
cache-stats:
    sccache --show-stats

# Reset sccache statistics
cache-reset:
    sccache --zero-stats

# Start sccache server
cache-start:
    sccache --start-server

# Stop sccache server
cache-stop:
    sccache --stop-server

# ── tools ────────────────────────────────────────────────────────────────────

# Bootstrap local Android tooling (adb + scrcpy)
bootstrap:
    npm run bootstrap:tools

# Verify local tooling is present
verify-tools:
    npm run verify:tools

# Install git hooks
hooks:
    npm run hooks:install

# ── ci helpers ───────────────────────────────────────────────────────────────

# CI test suite (build + unit tests only, no device)
ci: build test-unit test-rust-ci

# CI with desktop build verification
ci-desktop: ci desktop-check
