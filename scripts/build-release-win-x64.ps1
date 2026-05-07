param(
    [string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-HackArenaVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CargoTomlPath
    )

    $content = Get-Content -LiteralPath $CargoTomlPath
    foreach ($line in $content) {
        if ($line -match '^\s*version\s*=\s*"([^"]+)"\s*$') {
            return $Matches[1]
        }
    }

    throw "Could not find package version in $CargoTomlPath"
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Split-Path -Parent $scriptDir
$cargoTomlPath = Join-Path $repoRoot 'Cargo.toml'

if (-not $Version) {
    $Version = Get-HackArenaVersion -CargoTomlPath $cargoTomlPath
}

$targetTriple = 'x86_64-pc-windows-msvc'
$binaryName = 'hackarena.exe'
$releaseFileName = "hackarena-cli-v$Version-$targetTriple.exe"
$sourceBinaryPath = Join-Path $repoRoot "target\$targetTriple\release\$binaryName"
$deployDir = Join-Path $repoRoot "deploy\$Version"
$deployBinaryPath = Join-Path $deployDir $releaseFileName

Push-Location $repoRoot
try {
    cargo build --release --target $targetTriple
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $sourceBinaryPath)) {
    throw "Built binary not found at $sourceBinaryPath"
}

New-Item -ItemType Directory -Force -Path $deployDir | Out-Null
Copy-Item -LiteralPath $sourceBinaryPath -Destination $deployBinaryPath -Force

Write-Host "Built Windows x64 release:"
Write-Host "  $deployBinaryPath"
