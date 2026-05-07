param(
    [string]$Path,
    [string]$Version,
    [string]$OutputFile = 'SHA256SUMS.txt'
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

if ($Path -and $Version) {
    throw "Use either -Path or -Version, not both."
}

if (-not $Path) {
    if (-not $Version) {
        $Version = Get-HackArenaVersion -CargoTomlPath $cargoTomlPath
    }
    $Path = Join-Path $repoRoot "deploy\$Version"
}

if (-not [System.IO.Path]::IsPathRooted($Path)) {
    $Path = Join-Path $repoRoot $Path
}

$resolvedInput = Resolve-Path -LiteralPath $Path | Select-Object -ExpandProperty Path

if (Test-Path -LiteralPath $resolvedInput -PathType Leaf) {
    $parentDir = Split-Path -Parent $resolvedInput
    $outputPath = Join-Path $parentDir $OutputFile
    $fileName = Split-Path -Leaf $resolvedInput
    $hash = (Get-FileHash -LiteralPath $resolvedInput -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $fileName" | Set-Content -LiteralPath $outputPath -NoNewline
    Write-Host "Wrote checksums to:"
    Write-Host "  $outputPath"
    return
}

if (-not (Test-Path -LiteralPath $resolvedInput -PathType Container)) {
    throw "Path does not exist: $resolvedInput"
}

$outputPath = Join-Path $resolvedInput $OutputFile
$entries = Get-ChildItem -LiteralPath $resolvedInput -File |
    Where-Object { $_.Name -ne $OutputFile } |
    Sort-Object Name

if (-not $entries) {
    throw "No files found in directory: $resolvedInput"
}

$lines = foreach ($entry in $entries) {
    $hash = (Get-FileHash -LiteralPath $entry.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $($entry.Name)"
}

Set-Content -LiteralPath $outputPath -Value $lines
Write-Host "Wrote checksums to:"
Write-Host "  $outputPath"
