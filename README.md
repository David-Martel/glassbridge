# Pixel Android Control Framework

Reusable local framework for controlling Android devices with repo-local binaries,
a Fastify control plane, and a lightweight browser UI for live operations.

The backend is keyed by device serial so it can manage multiple approved devices
concurrently. Approved devices persist in the local SQLite registry at
`data/device-registry.db`.

## Quick start

```powershell
npm install
npm run bootstrap:tools
npm run dev
```

Server runs on `http://localhost:3030` and the web UI on `http://localhost:5173`.

Launch the Tauri desktop shell with:

```powershell
npm run desktop
```

Build the desktop binary shell with:

```powershell
npm run desktop:build
```

Validate the built application against the connected Android device with:

```powershell
npm run lint
npm test
npm run test:desktop
```

The repeatable suite split is:

- `npm run test:frontend` for frontend-only DOM and action wiring
- `npm run test:backend` for live backend validation against the connected device
- `npm run test:integration` for frontend rendering against live backend payloads from the connected device
- `npm run test:desktop` for Rust/Tauri desktop shell compilation checks
- `npm run test:ci` for device-free CI validation
