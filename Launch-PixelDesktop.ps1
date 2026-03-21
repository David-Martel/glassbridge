param(
  [switch]$BuildOnly,
  [switch]$CheckOnly,
  [switch]$StageOnly,
  [switch]$CacheStatsOnly,
  [switch]$ResetSccacheStats
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$desktopManifestPath = Join-Path $repoRoot "apps/web/src-tauri/Cargo.toml"
$desktopStagingDir = Join-Path $repoRoot "staging/desktop"

function Resolve-SccacheCommand {
  return Get-Command sccache -ErrorAction SilentlyContinue
}

function Get-FileSha256 {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Path
  )

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  $stream = [System.IO.File]::OpenRead($Path)
  try {
    return ([System.BitConverter]::ToString($sha256.ComputeHash($stream))).Replace("-", "")
  } finally {
    $stream.Dispose()
    $sha256.Dispose()
  }
}

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

function Ensure-DesktopCargoConfig {
  param(
    [bool]$UsesCargoTools
  )

  $localCargoDir = Join-Path $repoRoot ".cargo"
  $localCargoConfig = Join-Path $localCargoDir "config.toml"

  if (Test-Path -LiteralPath $localCargoConfig) {
    return
  }

  if ($UsesCargoTools -and (Get-Command Initialize-ProjectCargoConfig -ErrorAction SilentlyContinue)) {
    Write-Host "Generating local .cargo/config.toml from CargoTools machine defaults..."
    Initialize-ProjectCargoConfig -Path $repoRoot -Profile minimal | Out-Null
    return
  }

  if (-not (Test-Path -LiteralPath $localCargoDir)) {
    New-Item -ItemType Directory -Path $localCargoDir -Force | Out-Null
  }

  @"
# Generated fallback config for local desktop builds.
[build]
rustc-wrapper = "sccache"
target-dir = "target"

[env]
RUSTC_WRAPPER = "sccache"
CARGO_INCREMENTAL = "0"
"@ | Set-Content -LiteralPath $localCargoConfig -Encoding utf8
}

function Invoke-DesktopCargo {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Command,
    [string[]]$AdditionalArgs = @()
  )

  $mutex = $null
  $hasMutex = $false

  try {
    $mutex = [System.Threading.Mutex]::new($false, "Global\PixelAndroidControlDesktopCargo")
    Write-Host "Waiting for desktop Cargo lock..." -ForegroundColor DarkGray

    try {
      $hasMutex = $mutex.WaitOne([TimeSpan]::FromMinutes(30))
    } catch [System.Threading.AbandonedMutexException] {
      $hasMutex = $true
    }

    if (-not $hasMutex) {
      throw "Timed out waiting for the desktop Cargo build lock."
    }

    & cargo $Command @AdditionalArgs
    if ($LASTEXITCODE -ne 0) {
      throw "cargo $Command failed with exit code $LASTEXITCODE."
    }
  } finally {
    if ($hasMutex -and $null -ne $mutex) {
      $mutex.ReleaseMutex() | Out-Null
    }
    if ($null -ne $mutex) {
      $mutex.Dispose()
    }
  }
}

function Get-SccacheStats {
  $sccache = Resolve-SccacheCommand
  if (-not $sccache) {
    return $null
  }

  $output = & $sccache.Source --show-stats 2>$null
  if ($LASTEXITCODE -ne 0) {
    return $null
  }

  $stats = [ordered]@{}
  foreach ($line in $output) {
    if ($line -match "^\s*([^:]+?)\s{2,}(.+?)\s*$") {
      $stats[$matches[1].Trim()] = $matches[2].Trim()
    }
  }

  return [pscustomobject]$stats
}

function Get-SccacheStatValue {
  param(
    [object]$Stats,
    [Parameter(Mandatory = $true)]
    [string]$Name,
    [string]$Default = "n/a"
  )

  if ($null -eq $Stats) {
    return $Default
  }

  $property = $Stats.PSObject.Properties[$Name]
  if ($null -eq $property) {
    return $Default
  }

  $value = "$($property.Value)".Trim()
  if ([string]::IsNullOrWhiteSpace($value)) {
    return $Default
  }

  return $value
}

function Write-SccacheSummary {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Label,
    [object]$Stats
  )

  if ($null -eq $Stats) {
    return
  }

  $rustHits = Get-SccacheStatValue -Stats $Stats -Name "Cache hits (Rust)"
  if ($rustHits -eq "n/a") {
    $rustHits = Get-SccacheStatValue -Stats $Stats -Name "Cache hits"
  }
  $rustMisses = Get-SccacheStatValue -Stats $Stats -Name "Cache misses (Rust)"
  if ($rustMisses -eq "n/a") {
    $rustMisses = Get-SccacheStatValue -Stats $Stats -Name "Cache misses"
  }
  $rustHitRate = Get-SccacheStatValue -Stats $Stats -Name "Cache hits rate (Rust)"
  if ($rustHitRate -eq "n/a") {
    $rustHitRate = Get-SccacheStatValue -Stats $Stats -Name "Cache hits rate"
  }
  $compileRequests = Get-SccacheStatValue -Stats $Stats -Name "Compile requests"
  $nonCacheableCalls = Get-SccacheStatValue -Stats $Stats -Name "Non-cacheable calls"
  $cacheSize = Get-SccacheStatValue -Stats $Stats -Name "Cache size"

  Write-Host ("{0}: requests {1}, hits {2}, misses {3}, hit rate {4}, non-cacheable {5}, cache size {6}" -f $Label, $compileRequests, $rustHits, $rustMisses, $rustHitRate, $nonCacheableCalls, $cacheSize) -ForegroundColor DarkCyan
}

function Ensure-Sccache {
  param(
    [bool]$UsesCargoTools,
    [bool]$ResetStats
  )

  $sccache = Resolve-SccacheCommand
  if (-not $sccache) {
    Write-Warning "sccache was not found in PATH. Rust builds will continue without compiler caching."
    return $false
  }

  $env:RUSTC_WRAPPER = "sccache"
  $env:CARGO_BUILD_RUSTC_WRAPPER = "sccache"
  if (-not $env:SCCACHE_CACHE_COMPRESSION) {
    $env:SCCACHE_CACHE_COMPRESSION = "zstd"
  }
  if (-not $env:SCCACHE_CACHE_SIZE) {
    $env:SCCACHE_CACHE_SIZE = "30G"
  }
  if (-not $env:SCCACHE_DIRECT) {
    $env:SCCACHE_DIRECT = "true"
  }

  if ($UsesCargoTools -and (Get-Command Start-SccacheServer -ErrorAction SilentlyContinue)) {
    $started = Start-SccacheServer -MaxMemoryMB 4096
    if (-not $started) {
      Write-Warning "CargoTools was unable to start or verify the sccache server. Falling back to direct startup."
      & $sccache.Source --start-server 2>$null | Out-Null
      if ($LASTEXITCODE -ne 0) {
        Write-Warning "sccache --start-server failed with exit code $LASTEXITCODE."
        return $false
      }
    }
  } else {
    & $sccache.Source --start-server 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
      Write-Warning "sccache --start-server failed with exit code $LASTEXITCODE."
      return $false
    }
  }

  $stats = Get-SccacheStats
  if ($null -eq $stats) {
    Write-Warning "sccache is installed but its health check failed."
    return $false
  }

  if ($ResetStats) {
    & $sccache.Source --zero-stats 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
      Write-Warning "sccache did not accept --zero-stats."
      return $false
    }
    $stats = Get-SccacheStats
  }

  Write-SccacheSummary -Label "sccache ready" -Stats $stats
  return $true
}

function Invoke-PrebuildValidation {
  param(
    [bool]$UsesCargoTools
  )

  if (
    $UsesCargoTools -and
    $env:PIXEL_DESKTOP_DIAGNOSTICS -eq "1" -and
    (Get-Command Test-BuildEnvironment -ErrorAction SilentlyContinue)
  ) {
    Write-Host "Inspecting CargoTools build environment..."
    Test-BuildEnvironment | Out-Host
  }
}

function Stage-DesktopArtifact {
  param(
    [bool]$UsesCargoTools
  )

  $resolvedTargetDirectory = if (
    $UsesCargoTools -and
    (Get-Command Resolve-CargoTargetDirectory -ErrorAction SilentlyContinue)
  ) {
    Resolve-CargoTargetDirectory -ManifestPath $desktopManifestPath -Configuration Release
  } else {
    Join-Path $repoRoot "apps/web/src-tauri/target/release"
  }

  $artifactSource = Join-Path $resolvedTargetDirectory "glassbridge_desktop.exe"
  if (-not (Test-Path -LiteralPath $artifactSource)) {
    throw "Desktop executable not found at $artifactSource."
  }

  if (-not (Test-Path -LiteralPath $desktopStagingDir)) {
    New-Item -ItemType Directory -Path $desktopStagingDir -Force | Out-Null
  }

  $destinationPath = Join-Path $desktopStagingDir "glassbridge_desktop.exe"
  $manifestPath = "$destinationPath.buildinfo.json"
  $stagedWithCargoTools = $false

  if (
    $UsesCargoTools -and
    (Get-Command Publish-BuildArtifact -ErrorAction SilentlyContinue) -and
    (Get-Command Get-FileHash -ErrorAction SilentlyContinue)
  ) {
    try {
      $published = Publish-BuildArtifact `
        -SourcePath $artifactSource `
        -DestinationDirectory $desktopStagingDir `
        -DestinationFileName "glassbridge_desktop.exe" `
        -ArtifactKind "desktop-executable"

      if (
        $null -ne $published -and
        (Test-Path -LiteralPath $published.DestinationPath) -and
        (Test-Path -LiteralPath $published.ManifestPath)
      ) {
        $destinationPath = $published.DestinationPath
        $manifestPath = $published.ManifestPath
        $stagedWithCargoTools = $true
      } else {
        Write-Warning "CargoTools reported a publish result, but the staged artifact or manifest was missing. Falling back to direct copy."
      }
    } catch {
      Write-Warning "CargoTools artifact publishing failed: $_"
    }
  }

  if (-not $stagedWithCargoTools) {
    Copy-Item -LiteralPath $artifactSource -Destination $destinationPath -Force
    $stagedItem = Get-Item -LiteralPath $destinationPath -ErrorAction Stop
    $manifest = [ordered]@{
      artifactKind = "desktop-executable"
      fileName = $stagedItem.Name
      destinationPath = $destinationPath
      sourcePath = (Resolve-Path -LiteralPath $artifactSource).Path
      sizeBytes = $stagedItem.Length
      sha256 = Get-FileSha256 -Path $destinationPath
      copiedUtc = [DateTime]::UtcNow.ToString("o")
      publisher = "Launch-PixelDesktop.ps1"
      cargoToolsPublish = $false
    }
    $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $manifestPath -Encoding utf8
  }

  Write-Host "Staged desktop executable: $destinationPath" -ForegroundColor Green
  Write-Host "Artifact manifest: $manifestPath" -ForegroundColor DarkGreen

  $pdbSource = Join-Path $resolvedTargetDirectory "glassbridge_desktop.pdb"
  if (Test-Path -LiteralPath $pdbSource) {
    Copy-Item -LiteralPath $pdbSource -Destination (Join-Path $desktopStagingDir "glassbridge_desktop.pdb") -Force
    Write-Host "Staged debug symbols: $(Join-Path $desktopStagingDir 'glassbridge_desktop.pdb')" -ForegroundColor DarkGreen
  }
}

Push-Location $repoRoot
try {
  $usesCargoTools = Initialize-CargoToolsBridge
  if ($usesCargoTools) {
    Write-Host "CargoTools detected. Using CargoTools-managed Rust environment." -ForegroundColor Cyan
  }
  Ensure-DesktopCargoConfig -UsesCargoTools $usesCargoTools
  $usesSccache = Ensure-Sccache -UsesCargoTools $usesCargoTools -ResetStats $ResetSccacheStats
  Invoke-PrebuildValidation -UsesCargoTools $usesCargoTools

  if ($CacheStatsOnly) {
    if ($usesSccache) {
      Write-SccacheSummary -Label "sccache current stats" -Stats (Get-SccacheStats)
      return
    }

    throw "sccache is not available for cache stats inspection."
  }

  if ($StageOnly) {
    Stage-DesktopArtifact -UsesCargoTools $usesCargoTools
    if ($usesSccache) {
      Write-SccacheSummary -Label "sccache after staging" -Stats (Get-SccacheStats)
    }
    return
  }

  if ($CheckOnly) {
    Write-Host "Checking the Tauri desktop shell with CargoTools-compatible cargo settings..."
    Invoke-DesktopCargo -Command "check" -AdditionalArgs @(
      "--manifest-path",
      $desktopManifestPath
    )
    if ($usesSccache) {
      Write-SccacheSummary -Label "sccache after desktop check" -Stats (Get-SccacheStats)
    }
    return
  }

  # NOTE: The Node.js backend (apps/server) is NOT built for desktop mode.
  # main.rs now runs a self-contained Axum server in-process, so the Node.js
  # control plane is only required for browser-mode development (npm run dev).

  if ($BuildOnly) {
    Write-Host "Building the web frontend for the Tauri desktop shell..."
    npm run build --workspace apps/web
    Invoke-DesktopCargo -Command "build" -AdditionalArgs @(
      "--release",
      "--manifest-path",
      $desktopManifestPath
    )
    Stage-DesktopArtifact -UsesCargoTools $usesCargoTools
    if ($usesSccache) {
      Write-SccacheSummary -Label "sccache after desktop build" -Stats (Get-SccacheStats)
    }
    return
  }

  Write-Host "Launching the Tauri desktop shell..."
  npm run tauri:dev:raw --workspace apps/web
} finally {
  Pop-Location
}
