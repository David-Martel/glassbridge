param(
  # ---------- mode switches (mutually exclusive) ----------
  [switch]$Lint,         # lint + format (biome autofix + cargo fmt)
  [switch]$Check,        # lint + typecheck + cargo check (no build)
  [switch]$Build,        # full build (server + web + desktop release)
  [switch]$Test,         # test:unit + test:rust (deviceless)
  [switch]$TestAll,      # test:all including device tests (graceful skip)
  [switch]$TestDevice,   # device tests only (requires connected device)
  [switch]$Desktop,      # desktop shell build only (delegates to Launch-PixelDesktop.ps1)
  [switch]$Release,      # full release validation (everything)
  [switch]$Clean,        # clean build artifacts
  [switch]$Dev,          # start dev servers

  # ---------- modifiers ----------
  [switch]$Fix,          # apply autofixes (biome --write, cargo fmt)
  [switch]$Nextest,      # use cargo-nextest for Rust tests
  [switch]$NoSccache,    # skip sccache initialization
  [switch]$Verbose       # verbose output
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$tauriManifest = Join-Path $repoRoot "apps/web/src-tauri/Cargo.toml"
$nextestConfig = Join-Path $repoRoot ".config/nextest.toml"
$script:exitCode = 0

# ── CargoTools + sccache bootstrap ───────────────────────────────────────────

function Initialize-BuildEnvironment {
  $script:usesCargoTools = $false
  $script:usesSccache = $false

  # CargoTools
  if (Get-Module -ListAvailable CargoTools) {
    Import-Module CargoTools -ErrorAction Stop
    if (Get-Command Initialize-CargoEnv -ErrorAction SilentlyContinue) {
      Initialize-CargoEnv
    }
    $script:usesCargoTools = $true
    Write-Host "CargoTools loaded." -ForegroundColor Cyan
  }

  # .cargo/config.toml
  $localCargo = Join-Path $repoRoot ".cargo/config.toml"
  if (-not (Test-Path -LiteralPath $localCargo)) {
    if ($script:usesCargoTools -and (Get-Command Initialize-ProjectCargoConfig -ErrorAction SilentlyContinue)) {
      Initialize-ProjectCargoConfig -Path $repoRoot -Profile minimal | Out-Null
    } else {
      $dir = Join-Path $repoRoot ".cargo"
      if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
      @"
[build]
rustc-wrapper = "sccache"
target-dir = "target"

[env]
RUSTC_WRAPPER = "sccache"
CARGO_INCREMENTAL = "0"
"@ | Set-Content -LiteralPath $localCargo -Encoding utf8
    }
  }

  # sccache
  if (-not $NoSccache) {
    $scc = Get-Command sccache -ErrorAction SilentlyContinue
    if ($scc) {
      $env:RUSTC_WRAPPER = "sccache"
      $env:CARGO_BUILD_RUSTC_WRAPPER = "sccache"
      if (-not $env:SCCACHE_CACHE_COMPRESSION) { $env:SCCACHE_CACHE_COMPRESSION = "zstd" }
      if (-not $env:SCCACHE_CACHE_SIZE) { $env:SCCACHE_CACHE_SIZE = "30G" }
      if (-not $env:SCCACHE_DIRECT) { $env:SCCACHE_DIRECT = "true" }

      if ($script:usesCargoTools -and (Get-Command Start-SccacheServer -ErrorAction SilentlyContinue)) {
        Start-SccacheServer -MaxMemoryMB 4096 | Out-Null
      } else {
        & sccache --start-server 2>$null | Out-Null
      }

      $script:usesSccache = $true
      Write-Host "sccache ready." -ForegroundColor Cyan
    } else {
      Write-Warning "sccache not found. Rust builds will proceed without compiler caching."
    }
  }

  # rustfmt.toml (ensure exists for cargo fmt consistency)
  $fmtConfig = Join-Path $repoRoot "apps/web/src-tauri/rustfmt.toml"
  if (-not (Test-Path -LiteralPath $fmtConfig)) {
    "edition = `"2021`"" | Set-Content -LiteralPath $fmtConfig -Encoding utf8
  }
}

# ── Step runner ──────────────────────────────────────────────────────────────

function Invoke-Step {
  param(
    [Parameter(Mandatory)]
    [string]$Label,
    [Parameter(Mandatory)]
    [scriptblock]$Action
  )

  Write-Host ""
  Write-Host "--- $Label ---" -ForegroundColor White
  try {
    & $Action
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) {
      throw "$Label failed (exit code $LASTEXITCODE)."
    }
    Write-Host "  OK" -ForegroundColor Green
  } catch {
    Write-Host "  FAILED: $_" -ForegroundColor Red
    $script:exitCode = 1
    throw
  }
}

# ── Lint & format ────────────────────────────────────────────────────────────

function Invoke-Lint {
  if ($Fix) {
    Invoke-Step "Biome autofix" { npx biome check --write . }
    Invoke-Step "Biome unsafe fixes" { npx biome check --write --unsafe . }
    Invoke-Step "Cargo fmt" { cargo fmt --manifest-path $tauriManifest --all }
  } else {
    Invoke-Step "Biome check" { npx biome check . }
    Invoke-Step "Cargo fmt check" { cargo fmt --manifest-path $tauriManifest --all --check }
  }
}

# ── Typecheck ────────────────────────────────────────────────────────────────

function Invoke-Typecheck {
  Invoke-Step "TypeScript typecheck" { npm run typecheck }
}

# ── Cargo check / clippy ─────────────────────────────────────────────────────

function Invoke-CargoCheck {
  Invoke-Step "Cargo clippy" {
    cargo clippy --manifest-path $tauriManifest --all-targets -- -D warnings
  }
}

# ── Build ────────────────────────────────────────────────────────────────────

function Invoke-Build {
  Invoke-Step "Build server" { npm run build --workspace apps/server }
  Invoke-Step "Build web" { npm run build --workspace apps/web }
}

function Invoke-DesktopBuild {
  Invoke-Step "Desktop build" {
    pwsh -ExecutionPolicy Bypass -File (Join-Path $repoRoot "Launch-PixelDesktop.ps1") -BuildOnly
  }
}

# ── Test ─────────────────────────────────────────────────────────────────────

function Invoke-TestUnit {
  Invoke-Step "Frontend unit tests (vitest)" { npx vitest run tests/frontend-ui.test.ts }
}

function Invoke-TestRust {
  if ($Nextest -and (Get-Command cargo-nextest -ErrorAction SilentlyContinue)) {
    $profileArg = if (Test-Path -LiteralPath $nextestConfig) { @("--config-file", $nextestConfig) } else { @() }
    Invoke-Step "Rust tests (nextest)" {
      cargo nextest run --manifest-path $tauriManifest --all-features @profileArg
    }
  } else {
    Invoke-Step "Rust tests (cargo test)" {
      cargo test --manifest-path $tauriManifest --all-features
    }
  }
}

function Invoke-TestDevice {
  Invoke-Step "Build server for device tests" { npm run build --workspace apps/server }
  Invoke-Step "Device smoke tests" { node --test tests/device-smoke.test.mjs }
}

function Invoke-TestIntegration {
  Invoke-Step "Build server for integration tests" { npm run build --workspace apps/server }
  Invoke-Step "Integration tests (vitest)" { npx vitest run tests/frontend-backend.integration.test.ts }
}

function Invoke-TestAll {
  Invoke-Build
  Invoke-TestUnit
  Invoke-TestRust
  Invoke-TestDevice
  Invoke-TestIntegration
}

# ── Secrets ──────────────────────────────────────────────────────────────────

function Invoke-Secrets {
  param([switch]$Full)
  Invoke-Step "Secrets: staged" { npm run secrets:staged }
  if ($Full) {
    Invoke-Step "Secrets: repo" { npm run secrets:repo }
    Invoke-Step "Secrets: history" { npm run secrets:history }
  }
}

# ── Clean ────────────────────────────────────────────────────────────────────

function Invoke-Clean {
  Invoke-Step "Clean Node build artifacts" {
    $dirs = @("apps/server/dist", "apps/web/dist")
    foreach ($d in $dirs) {
      $p = Join-Path $repoRoot $d
      if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
    }
  }
  Invoke-Step "Clean Rust target" {
    cargo clean --manifest-path $tauriManifest
  }
  Invoke-Step "Clean staging" {
    $staging = Join-Path $repoRoot "staging/desktop"
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
  }
}

# ── Main dispatch ────────────────────────────────────────────────────────────

Push-Location $repoRoot
try {
  # Default: if no switch provided, run Check
  $noSwitch = -not ($Lint -or $Check -or $Build -or $Test -or $TestAll -or $TestDevice -or
    $Desktop -or $Release -or $Clean -or $Dev)

  Initialize-BuildEnvironment

  if ($Clean) {
    Invoke-Clean
    return
  }

  if ($Dev) {
    npm run dev
    return
  }

  if ($Lint -or $noSwitch) {
    Invoke-Lint
    if ($Lint) { return }
  }

  if ($Check -or $noSwitch) {
    Invoke-Typecheck
    Invoke-CargoCheck
    if ($Check) { return }
  }

  if ($Test) {
    Invoke-Build
    Invoke-TestUnit
    Invoke-TestRust
    return
  }

  if ($TestDevice) {
    Invoke-TestDevice
    return
  }

  if ($TestAll) {
    Invoke-TestAll
    return
  }

  if ($Desktop) {
    Invoke-DesktopBuild
    return
  }

  if ($Build) {
    Invoke-Build
    Invoke-DesktopBuild
    return
  }

  if ($Release) {
    Invoke-Secrets -Full
    Invoke-Lint
    Invoke-Typecheck
    Invoke-CargoCheck
    Invoke-TestAll
    Invoke-DesktopBuild
    Write-Host ""
    Write-Host "Release validation complete." -ForegroundColor Green
    return
  }

  if ($noSwitch) {
    Write-Host ""
    Write-Host "Default check complete (lint + typecheck + clippy)." -ForegroundColor Green
  }
} catch {
  Write-Host ""
  Write-Host "Build failed: $_" -ForegroundColor Red
  exit 1
} finally {
  if ($script:usesSccache) {
    $statsOutput = & sccache --show-stats 2>$null
    if ($statsOutput) {
      Write-Host ""
      Write-Host "sccache summary:" -ForegroundColor DarkCyan
      $statsOutput | Where-Object { $_ -match "(hits|misses|rate|size|requests)" } | ForEach-Object {
        Write-Host "  $_" -ForegroundColor DarkGray
      }
    }
  }
  Pop-Location
}
