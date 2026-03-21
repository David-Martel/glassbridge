# tools/pre-push.ps1
#
# Pre-push hook — medium gate (target: <3 min on a warm machine).
#
# Checks performed:
#   1. Secret scan on full repo
#   2. Biome lint
#   3. TypeScript typecheck
#   4. Build (server + web)
#   5. Unit tests (vitest, no device)
#   6. Integration tests (vitest + real backend)
#   7. Rust fmt check
#   8. Rust clippy
#   9. Desktop check (Rust debug build)
#
# Device-dependent tests are NOT run here — they require a connected Android
# device and will fail in environments without hardware attached.
# Run them manually when a device is available:
#   npm run test:device
#
# To hard-fail when no device is connected (e.g., on a device lab machine):
#   $env:GLASSBRIDGE_REQUIRE_DEVICE = "1"; npm run test:device

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
  Write-Host "==> [1/9] Secret scan (full repo)..." -ForegroundColor Cyan
  npm run secrets:repo
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [2/9] Lint (biome)..." -ForegroundColor Cyan
  npm run lint
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [3/9] Typecheck..." -ForegroundColor Cyan
  npm run typecheck
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [4/9] Build (server + web)..." -ForegroundColor Cyan
  npm run build
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [5/9] Unit tests..." -ForegroundColor Cyan
  npm run test:unit
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [6/9] Integration tests..." -ForegroundColor Cyan
  npm run test:integration
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [7/9] Rust format check..." -ForegroundColor Cyan
  npm run rust:fmt:check
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [8/9] Rust clippy..." -ForegroundColor Cyan
  npm run rust:check
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [9/9] Desktop check (Rust debug build)..." -ForegroundColor Cyan
  npm run desktop:check
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "Pre-push checks passed." -ForegroundColor Green
  exit 0
} finally {
  Pop-Location
}
