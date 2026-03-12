param(
  [switch]$BuildOnly,
  [switch]$CheckOnly
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

function Initialize-CargoToolsBridge {
  if (-not (Get-Module -ListAvailable CargoTools)) {
    return $false
  }

  Import-Module CargoTools -ErrorAction Stop
  if (Get-Command Initialize-CargoEnv -ErrorAction SilentlyContinue) {
    Initialize-CargoEnv
  }
  return $true
}

function Invoke-DesktopCargo {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Command,
    [string[]]$AdditionalArgs = @()
  )

  & cargo $Command @AdditionalArgs
  if ($LASTEXITCODE -ne 0) {
    throw "cargo $Command failed with exit code $LASTEXITCODE."
  }
}

Push-Location $repoRoot
try {
  $usesCargoTools = Initialize-CargoToolsBridge
  if ($usesCargoTools) {
    Write-Host "CargoTools detected. Using CargoTools-managed Rust environment." -ForegroundColor Cyan
  }

  if ($CheckOnly) {
    Write-Host "Checking the Tauri desktop shell with CargoTools-compatible cargo settings..."
    Invoke-DesktopCargo -Command "check" -AdditionalArgs @(
      "--manifest-path",
      "apps/web/src-tauri/Cargo.toml"
    )
    return
  }

  Write-Host "Building the backend control plane for desktop launch..."
  npm run build --workspace apps/server

  if ($BuildOnly) {
    Write-Host "Building the Tauri desktop shell with CargoTools-compatible cargo settings..."
    npm run build --workspace apps/web
    Invoke-DesktopCargo -Command "build" -AdditionalArgs @(
      "--release",
      "--manifest-path",
      "apps/web/src-tauri/Cargo.toml"
    )
    return
  }

  Write-Host "Launching the Tauri desktop shell..."
  npm run tauri:dev --workspace apps/web
} finally {
  Pop-Location
}
