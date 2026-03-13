param(
  [switch]$Staged,
  [switch]$Repo,
  [switch]$History
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $repoRoot ".gitleaks.toml"
$outputRoot = Join-Path $repoRoot "output"

if (-not ($Staged -or $Repo -or $History)) {
  $Repo = $true
}

function Get-TargetFiles {
  if ($History) {
    return git ls-files
  }

  if ($Staged) {
    return git diff --cached --name-only --diff-filter=ACMR
  }

  return git ls-files
}

function Test-BlockedFiles {
  param([string[]]$Files)

  $blockedRegex = @(
    '(^|/|\\)\.env(\..+)?$',
    '\.(pem|pfx|p12|cer|crt|key|kdbx)$',
    '(^|/|\\)id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$'
  )

  foreach ($file in $Files) {
    if ([string]::IsNullOrWhiteSpace($file)) {
      continue
    }

    foreach ($pattern in $blockedRegex) {
      if ($file -match $pattern) {
        throw "Blocked sensitive file path detected: $file"
      }
    }
  }
}

Push-Location $repoRoot
try {
  if (-not (Get-Command gitleaks -ErrorAction SilentlyContinue)) {
    throw "gitleaks is required for secret scanning but was not found in PATH."
  }

  if (-not (Test-Path -LiteralPath $outputRoot)) {
    New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
  }

  $targetFiles = @(Get-TargetFiles)
  Test-BlockedFiles -Files $targetFiles

  if ($History) {
    Write-Host "Scanning git history with gitleaks..." -ForegroundColor Cyan
    & gitleaks git . --redact --config $configPath --report-format sarif --report-path (Join-Path $outputRoot "gitleaks-history.sarif") --log-opts="--all"
  } elseif ($Staged) {
    Write-Host "Scanning staged files with gitleaks..." -ForegroundColor Cyan
    & gitleaks git . --redact --config $configPath --report-format sarif --report-path (Join-Path $outputRoot "gitleaks-staged.sarif") --staged
  } else {
    Write-Host "Scanning repository contents with gitleaks..." -ForegroundColor Cyan
    & gitleaks git . --redact --config $configPath --report-format sarif --report-path (Join-Path $outputRoot "gitleaks-repo.sarif") --log-opts="--all"
  }

  if ($LASTEXITCODE -ne 0) {
    throw "gitleaks failed with exit code $LASTEXITCODE."
  }
} finally {
  Pop-Location
}
