# GlassBridge

GlassBridge is a local-first Android device access framework for desktop
operators. It combines a Fastify control plane, a responsive browser UI, and a
Tauri desktop shell around real `adb` and `scrcpy` workflows so one workspace
can manage multiple approved devices concurrently.

The production path uses real device tooling only. Local approval state is kept
in a SQLite registry under `data/`, which is intentionally ignored by git.

## What it does

- discovers connected Android devices and tracks them by serial
- stores an approved-device fleet for fast multi-device reuse
- renders live screenshots and supports click-to-tap interaction
- provides quick keys, shell execution, package browsing, and app launch/stop
- launches low-latency `scrcpy` sessions from the same control surface
- packages the same UX into a Tauri desktop shell for Windows operators

## Workspace layout

- `apps/server`: Fastify backend, adb/scrcpy orchestration, device registry
- `apps/web`: Vite frontend and Tauri desktop shell
- `tools`: bootstrap, validation, hook, and release automation
- `tests`: frontend, integration, live-device, and Playwright validation
- `docs`: product plan and architecture notes

## Quick start

```powershell
npm install
npm run bootstrap:tools
npm run hooks:install
npm run dev
```

Server default: `http://localhost:3030`  
Web UI default: `http://localhost:5173`

## Desktop workflow

Use the root launcher so CargoTools, `.cargo` config, `sccache`, and staging
stay consistent:

```powershell
npm run desktop
npm run desktop:check
npm run desktop:build
```

Built desktop artifacts are staged locally at:

```text
staging/desktop/glassbridge_desktop.exe
```

## Validation

```powershell
npm run lint
npm run typecheck
npm test
npm run test:desktop
```

Fast paths:

- `npm run validate:staged` for pre-commit validation
- `npm run validate:release` for pre-push validation
- `npm run secrets:staged` for staged-content scanning
- `npm run secrets:repo` for current-repo scanning
- `npm run secrets:history` for git-history scanning

Repeatable suite split:

- `npm run test:frontend` for DOM and UI wiring
- `npm run test:backend` for live backend validation against a connected device
- `npm run test:integration` for frontend/backend contract coverage
- `npm run test:desktop` for the desktop shell build path
- `npm run test:playwright:headed` for non-headless UI proof runs

## Tooling and build pipeline

- Repo-local Android tooling is bootstrapped into `vendor/`
- Desktop builds are driven by `Launch-PixelDesktop.ps1`
- CargoTools is used when present and plain Cargo is used as a fallback
- `sccache` is started and reported explicitly in the desktop pipeline
- Desktop release binaries are copied into `staging/desktop/` with a local build manifest

## Security and publication hygiene

- `gitleaks` runs in pre-commit, pre-push, and CI
- local device registry databases are ignored
- Playwright screenshots and staged binaries are ignored
- `.cargo/config.toml` remains machine-local

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md),
[docs/RELEASE-TODO.md](docs/RELEASE-TODO.md), and
[docs/android-control-plan.md](docs/android-control-plan.md) for more.
