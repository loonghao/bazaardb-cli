param(
    [Parameter(Mandatory = $true)]
    [string]$Target,
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$outputRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $OutputDir))
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null

$isWindowsTarget = $Target -like "*-windows-*"
$binaryName = if ($isWindowsTarget) { "bazaardb-cli.exe" } else { "bazaardb-cli" }
$binaryPath = Join-Path $repoRoot "target/$Target/release/$binaryName"
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "Release binary not found: $binaryPath"
}

$stage = Join-Path $outputRoot (".stage-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Copy-Item -LiteralPath $binaryPath -Destination (Join-Path $stage $binaryName)
    Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination $stage
    New-Item -ItemType Directory -Path (Join-Path $stage "profiles") | Out-Null
    Copy-Item -LiteralPath (Join-Path $repoRoot "profiles/bazaardb-cua.json") -Destination (Join-Path $stage "profiles")
    Set-Content -LiteralPath (Join-Path $stage "VERSION") -Value $Version -NoNewline

    if ($isWindowsTarget) {
        $archive = Join-Path $outputRoot "bazaardb-cli-v$Version-$Target.zip"
        Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -CompressionLevel Optimal -Force
    }
    else {
        $archive = Join-Path $outputRoot "bazaardb-cli-v$Version-$Target.tar.gz"
        & tar -C $stage -czf $archive .
        if ($LASTEXITCODE -ne 0) {
            throw "tar failed with exit code $LASTEXITCODE"
        }
    }
    Write-Output $archive
}
finally {
    $resolvedStage = [System.IO.Path]::GetFullPath($stage)
    if (-not $resolvedStage.StartsWith($outputRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove stage outside output directory: $resolvedStage"
    }
    if (Test-Path -LiteralPath $resolvedStage) {
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
}
