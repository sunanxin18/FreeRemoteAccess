param(
    [switch]$SkipNativeVerification
)

$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$DistDir = Join-Path $RepoRoot 'dist\windows'
$WorkDir = Join-Path $RepoRoot 'target\package\windows'
$ManifestTool = Join-Path $RepoRoot 'packaging\package_manifest.py'
$Version = (& python $ManifestTool --repo $RepoRoot version).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($Version)) {
    throw 'cargo_metadata_version_failed'
}
$MsiVersion = (& python $ManifestTool --repo $RepoRoot msi-version).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($MsiVersion)) {
    throw 'msi_product_version_failed'
}
$ArtifactPrefix = "FreeRemoteAccess-$Version-windows-x64"

function Assert-SafeCleanupRoot([string]$Path, [string]$ExpectedRelative) {
    $FullPath = [IO.Path]::GetFullPath($Path)
    $Separators = [char[]]@([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $RootPrefix = [IO.Path]::GetFullPath($RepoRoot).TrimEnd($Separators) + [IO.Path]::DirectorySeparatorChar
    if (!$FullPath.StartsWith($RootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'package_cleanup_root_invalid'
    }
    $Relative = $FullPath.Substring($RootPrefix.Length)
    if ($Relative -ne $ExpectedRelative) {
        throw 'package_cleanup_root_invalid'
    }
    if (Test-Path -LiteralPath $FullPath) {
        $Entries = @((Get-Item -Force -LiteralPath $FullPath)) + @(
            Get-ChildItem -Force -LiteralPath $FullPath -Recurse
        )
        if ($Entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) {
            throw 'package_cleanup_reparse_point_rejected'
        }
    }
    return $FullPath
}

foreach ($Cleanup in @(
    [pscustomobject]@{ Path = $DistDir; Relative = 'dist\windows' },
    [pscustomobject]@{ Path = $WorkDir; Relative = 'target\package\windows' }
)) {
    $Path = Assert-SafeCleanupRoot $Cleanup.Path $Cleanup.Relative
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
    }
}
New-Item -ItemType Directory -Force -Path $DistDir, $WorkDir | Out-Null

cargo fetch --locked --manifest-path (Join-Path $RepoRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo_locked_fetch_failed' }
$FdkMetadata = (& python $ManifestTool --repo $RepoRoot prepare-fdk --dest (Join-Path $DistDir 'THIRD_PARTY'))
if ($LASTEXITCODE -ne 0) { throw 'fdk_supply_chain_gate_failed' }
$FdkInfo = $FdkMetadata | ConvertFrom-Json
$FdkDirectoryName = Split-Path (Split-Path (Split-Path $FdkInfo.source_archive -Parent) -Parent) -Leaf
$FdkArchiveName = Split-Path $FdkInfo.source_archive -Leaf

Push-Location $RepoRoot
$PreviousRustFlags = $env:RUSTFLAGS
try {
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --locked --release --features gui --bin freeremotedesk --bin freeremoteaccess-gui
    if ($LASTEXITCODE -ne 0) { throw 'rust_release_build_failed' }

    $GuiSource = Join-Path $RepoRoot 'target\release\freeremoteaccess-gui.exe'
    $CliSource = Join-Path $RepoRoot 'target\release\freeremotedesk.exe'
    $GuiArtifact = Join-Path $DistDir "$ArtifactPrefix.exe"
    Copy-Item -LiteralPath $GuiSource -Destination $GuiArtifact -Force

    $PortableRoot = Join-Path $WorkDir 'portable\FreeRemoteAccess'
    New-Item -ItemType Directory -Force -Path $PortableRoot | Out-Null
    Copy-Item -LiteralPath $GuiSource -Destination (Join-Path $PortableRoot 'FreeRemoteAccess.exe')
    Copy-Item -LiteralPath $CliSource -Destination (Join-Path $PortableRoot 'freeremotedesk-cli.exe')
    Copy-Item -LiteralPath (Join-Path $DistDir 'THIRD_PARTY') -Destination $PortableRoot -Recurse
    $PortableZip = Join-Path $DistDir "$ArtifactPrefix-portable.zip"
    Compress-Archive -LiteralPath $PortableRoot -DestinationPath $PortableZip -CompressionLevel Optimal

    $MsiRoot = Join-Path $WorkDir 'msi-root'
    New-Item -ItemType Directory -Force -Path $MsiRoot | Out-Null
    Copy-Item -LiteralPath $GuiSource -Destination (Join-Path $MsiRoot 'FreeRemoteAccess.exe')
    Copy-Item -LiteralPath $CliSource -Destination (Join-Path $MsiRoot 'freeremotedesk-cli.exe')
    Copy-Item -LiteralPath (Join-Path $DistDir 'THIRD_PARTY') -Destination $MsiRoot -Recurse
    $MsiArtifact = Join-Path $DistDir "$ArtifactPrefix.msi"
    wix build -arch x64 `
        -d "SourceDir=$MsiRoot" `
        -d "PackageVersion=$MsiVersion" `
        -d "FdkDirectoryName=$FdkDirectoryName" `
        -d "FdkArchiveName=$FdkArchiveName" `
        -pdbtype none `
        -o $MsiArtifact `
        (Join-Path $PSScriptRoot 'wix\main.wxs')
    if ($LASTEXITCODE -ne 0) { throw 'wix_msi_build_failed' }

    python $ManifestTool --repo $RepoRoot write `
        --dist $DistDir --platform windows --arch x64 `
        --support-root (Join-Path $DistDir 'THIRD_PARTY') `
        --artifact "gui-exe=$GuiArtifact" `
        --artifact "portable-zip=$PortableZip" `
        --artifact "msi=$MsiArtifact"
    if ($LASTEXITCODE -ne 0) { throw 'artifact_manifest_write_failed' }
    python $ManifestTool --repo $RepoRoot verify --manifest (Join-Path $DistDir 'artifact-manifest.json')
    if ($LASTEXITCODE -ne 0) { throw 'artifact_manifest_verify_failed' }

    if (!$SkipNativeVerification) {
        & (Join-Path $PSScriptRoot 'verify-package.ps1') -DistDir $DistDir -SkipLifecycle
    }
} finally {
    if ($null -eq $PreviousRustFlags) {
        Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    } else {
        $env:RUSTFLAGS = $PreviousRustFlags
    }
    Pop-Location
}
