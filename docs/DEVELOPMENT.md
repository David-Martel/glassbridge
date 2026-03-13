# GlassBridge Development

## Local Setup

```powershell
npm install
npm run bootstrap:tools
npm run hooks:install
```

## Core Workflows

- `npm run dev` for the browser-first workflow
- `npm run desktop` for the desktop shell
- `npm run desktop:check` for the Tauri/Rust verification path
- `npm run desktop:build` for a staged Windows desktop artifact

## Security And Release Checks

Run these before you publish or open a public pull request:

```powershell
npm run secrets:staged
npm run secrets:repo
npm run secrets:history
npm run validate:staged
npm run validate:release
```

## Artifact Staging

The canonical artifact pipeline lives in `Launch-PixelDesktop.ps1`. It:

- initializes CargoTools when present
- starts and reports `sccache`
- serializes Cargo access with a named mutex
- stages desktop artifacts into `staging/desktop/`
- writes a `.buildinfo.json` manifest beside the staged executable

## CI

GitHub Actions covers:

- `gitleaks` secret scanning
- Node lint and device-free frontend validation
- Windows desktop build verification and artifact upload
