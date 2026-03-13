# GlassBridge Product Plan

## Goals

Build a reusable local framework for communicating with Android phones with:

- fast device discovery and command execution through `adb`
- low-latency live interaction through `scrcpy`
- app-level control for launch, stop, package browsing, and shell workflows
- contextual help UX so an operator can recover quickly when the device or cable state changes
- repo-local copies of required tooling so the framework runs without global setup

## Delivery Plan

1. Create a local toolchain bootstrapper that downloads official Windows binaries.
2. Expose a reusable Node/TypeScript control layer for `adb` and external tool launch.
3. Add a Fastify API for device inventory, quick controls, shell access, and app actions.
4. Add a browser UI with operations-focused panels and built-in guidance.
5. Add a device-facing test suite for the connected Pixel 10 Pro XL.
6. Verify the local install path with dependency install, binary bootstrap, build, and tests.
7. Persist approved devices locally so multi-device sessions can be reused without re-discovery friction.

## Proposed UX/UI Elements

### Mission Header

- tool health chips for `adb` and `scrcpy`
- device count and selected device state
- one-click actions for refresh and tool bootstrap

### Device Roster

- card per connected device showing serial, model, state, and transport id
- explicit state badges for `device`, `offline`, and `unauthorized`
- approval badge and quick-save action backed by the local device registry

### Approved Fleet

- saved approved-device list with alias, last-seen time, and reuse-ready serial targeting
- designed so the control plane can address multiple approved devices concurrently by serial

### Live Control Panel

- `Low Latency`, `Balanced`, and `High Quality` launch presets for `scrcpy`
- brief descriptions that explain bitrate and frame-rate tradeoffs

### Quick Interaction Strip

- large high-frequency action buttons: `Home`, `Back`, `Recents`, `Wake`, `Power`
- tap and text injection controls for rapid testing

### App Workspace

- searchable third-party package list
- actions to launch apps, stop apps, and refresh inventory

### Command Console

- raw `adb shell` command form
- response panel with scrollback for advanced workflows

### Guided Help Rail

- onboarding steps for USB debugging and RSA authorization
- recovery playbooks for `unauthorized`, `offline`, and missing-binary states

## Local Binaries

- Android SDK Platform Tools:
  `https://dl.google.com/android/repository/platform-tools-latest-windows.zip`
- scrcpy:
  `https://github.com/Genymobile/scrcpy/releases/download/v3.3.4/scrcpy-win64-v3.3.4.zip`
