$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$DistDir = Join-Path $RepoRoot 'dist\windows'
$SourceDir = Join-Path $RepoRoot 'target\release'
$Version = '0.1.0'
$ArtifactPrefix = "FreeRemoteAccess-$Version-windows-x64"
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

Push-Location $RepoRoot
try {
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --locked --release --no-default-features --features gui
    if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
    Copy-Item -LiteralPath (Join-Path $SourceDir 'freeremotedesk.exe') `
        -Destination (Join-Path $DistDir "$ArtifactPrefix.exe") -Force
    wix build -arch x64 -d "SourceDir=$SourceDir" `
        -pdbtype none `
        -o (Join-Path $DistDir "$ArtifactPrefix.msi") `
        (Join-Path $PSScriptRoot 'wix\main.wxs')
    if ($LASTEXITCODE -ne 0) { throw 'WiX MSI build failed' }
    & (Join-Path $RepoRoot 'packaging\check-artifact.ps1') `
        -Path (Join-Path $DistDir "$ArtifactPrefix.exe") -Version $Version -Platform windows -Arch x64
    & (Join-Path $RepoRoot 'packaging\check-artifact.ps1') `
        -Path (Join-Path $DistDir "$ArtifactPrefix.msi") -Version $Version -Platform windows -Arch x64
} finally {
    Pop-Location
}
