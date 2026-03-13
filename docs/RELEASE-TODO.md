# GlassBridge Release Todo

## Before Public Release

- [ ] Evaluate running the Fastify backend directly under Bun instead of Node for local development and packaged desktop launches.
- [ ] Benchmark `bun run` and `bun build --compile` against the current `tsx` + `tsc` backend path, especially for `adb`-heavy shell orchestration and startup latency.
- [ ] Decide whether the backend should stay on Node for maximum package compatibility or move to a Bun-compiled runtime for smaller/faster local deployment.
- [ ] If Bun is adopted, add parallel `bun` scripts and CI validation before switching the default workflow.
- [ ] Remove nested PowerShell host noise from `npm`-invoked desktop scripts by simplifying how root scripts invoke the launcher.
- [ ] Review whether the desktop shell should stage a signed or versioned release artifact in addition to the local `.buildinfo.json` manifest.
- [ ] Add a public screenshot or animated capture for the README once a sanitized device session is available.
- [ ] Decide whether to publish a tagged desktop-release workflow artifact on GitHub Releases in addition to Actions artifacts.

## Nice To Have

- [ ] Add a Bun-based backend smoke path to compare CPU and memory behavior during multi-device polling.
- [ ] Add a repo-local performance profile script for screenshot cadence, shell latency, and tap round-trip timing.
- [ ] Consider a single `npm run release:ready` command that chains secret scans, validation, desktop build, and artifact staging.
