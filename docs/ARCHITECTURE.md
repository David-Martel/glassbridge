# GlassBridge Architecture

## Runtime layers

1. `apps/server`
   Fastify backend exposing device inventory, screenshots, quick actions,
   shell execution, app controls, and persistent approved-device storage.
2. `apps/web`
   Vite frontend that renders device state, live screenshots, interactive
   controls, and operator guidance.
3. `apps/web/src-tauri`
   Tauri desktop shell that packages the same frontend for Windows operators.

## Device access model

- `adb` is the canonical transport for screenshots, shell commands, key events,
  app launch/stop, and tap injection.
- `scrcpy` handles low-latency mirrored control sessions outside the browser preview.
- The backend is keyed by device serial so multiple approved devices can be addressed concurrently.

## Local persistence

- Approved device state is stored in `data/device-registry.db`.
- The database is local-only and ignored by git.

## Desktop build path

- `Launch-PixelDesktop.ps1` is the supported entrypoint.
- CargoTools is used when available on the host.
- `.cargo/config.toml` is generated locally when needed.
- `sccache` is started explicitly and surfaced in build output.
- Release artifacts are copied into `staging/desktop/` with build metadata.

## Validation path

- Hooks call staged or full secret scans before validation.
- `validate:staged` covers lint, typecheck, frontend tests, and desktop check.
- `validate:release` covers secret scans, lint, typecheck, tests, and desktop validation.
- GitHub Actions runs secret scanning, Linux validation, and Windows desktop shell checks.
