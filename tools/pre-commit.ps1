# tools/pre-commit.ps1
#
# Pre-commit hook — fast gate (target: <30 s on a warm machine).
#
# Checks performed:
#   1. Secret scan on staged files only
#   2. Biome lint
#   3. TypeScript typecheck
#   4. Unit tests (vitest, no device, no network)
#
# Intentionally excluded:
#   - desktop:check  (Rust compile cycle is too slow for every commit; runs in pre-push)
#   - test:device    (requires hardware; run manually with `npm run test:device`)
#   - test:frontend  (superseded by test:unit)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
  Write-Host "==> [1/4] Secret scan (staged files)..." -ForegroundColor Cyan
  npm run secrets:staged
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [2/4] Lint (biome)..." -ForegroundColor Cyan
  npm run lint
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [3/4] Typecheck..." -ForegroundColor Cyan
  npm run typecheck
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "==> [4/4] Unit tests..." -ForegroundColor Cyan
  npm run test:unit
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

  Write-Host "Pre-commit checks passed." -ForegroundColor Green
  exit 0
} finally {
  Pop-Location
}
