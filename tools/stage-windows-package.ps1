[CmdletBinding()]
param(
    [string]$PackageRoot = "target/package-test",
    [string]$Application = "target/release/freeremotedesk-windows.exe",
    [string]$CodecSource = ".codex-target/ffmpeg-8.1.2/windows-x86_64/Release/codec",
    [string]$BuildProvenance = ".codex-target/ffmpeg-8.1.2/windows-x86_64/Release/build-provenance.txt",
    [string]$CorrespondingSourceAsset = ".codex-target/ffmpeg-8.1.2/release-assets/FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip",
    [string]$GitCommit,
    [string]$BuildId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$ApprovedDllNames = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
$CodecRelativeDirectory = "codecs/ffmpeg-8.1.2/windows-x86_64"
$CorrespondingSourceFileName = "FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"
$ExpectedSourceUrl = "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz"
$ExpectedArchiveSha256 = "464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C"
$ExpectedConfigureArguments = @(
    "--arch=x86_64", "--target-os=mingw32", "--cross-prefix=x86_64-w64-mingw32-",
    "--disable-static", "--enable-shared", "--disable-programs", "--disable-doc",
    "--disable-everything", "--enable-decoder=hevc", "--enable-parser=hevc",
    "--enable-protocol=file", "--disable-gpl", "--disable-nonfree", "--disable-version3",
    "--disable-autodetect", "--disable-network", "--disable-x86asm", "--disable-debug",
    "--enable-stripping"
)

function Resolve-RepoPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $RepoRoot $Path))
}

function Assert-UnderRepoRoot([string]$Path) {
    $root = $RepoRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $candidate = [IO.Path]::GetFullPath($Path)
    if (-not $candidate.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        throw "package staging 目标必须位于仓库目录内: $candidate"
    }
}

function Assert-ExactFileSet([string]$Directory, [string[]]$Expected) {
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        throw "目录不存在: $Directory"
    }
    $children = @(Get-ChildItem -LiteralPath $Directory -Force)
    if (@($children | Where-Object { $_.PSIsContainer }).Count -ne 0) {
        throw "目录包含未批准的子目录: $Directory"
    }
    $actualKey = ((@($children.Name) | Sort-Object) -join "|")
    $expectedKey = (($Expected | Sort-Object) -join "|")
    if ($actualKey -cne $expectedKey) {
        throw "目录文件集合不匹配；实际 [$($children.Name -join ', ')]，要求 [$($Expected -join ', ')]"
    }
}

function Get-PayloadSha256($Entries) {
    $records = @($Entries | Sort-Object { [string]$_.path } | ForEach-Object {
        "$([string]$_.path)`0$([string]$_.sha256)`n"
    }) -join ""
    $bytes = [Text.Encoding]::UTF8.GetBytes($records)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString("X2") }) -join "")
    }
    finally {
        $sha.Dispose()
    }
}

$package = Resolve-RepoPath $PackageRoot
$applicationPath = Resolve-RepoPath $Application
$codecSourcePath = Resolve-RepoPath $CodecSource
$provenancePath = Resolve-RepoPath $BuildProvenance
$sourceAssetPath = Resolve-RepoPath $CorrespondingSourceAsset
$templatePath = Join-Path $RepoRoot "packaging\windows\ffmpeg-manifest.json"
$licensesSource = Join-Path $RepoRoot "packaging\windows\licenses"
$verifier = Join-Path $RepoRoot "tools\verify-windows-package.ps1"
$releaseAssetsDirectory = Join-Path $RepoRoot "target\release-assets"
$releaseAsset = Join-Path $releaseAssetsDirectory $CorrespondingSourceFileName

Assert-UnderRepoRoot $package
Assert-UnderRepoRoot $releaseAssetsDirectory
foreach ($requiredFile in @($applicationPath, $provenancePath, $sourceAssetPath, $templatePath, $verifier)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "package staging 输入不存在: $requiredFile"
    }
}
Assert-ExactFileSet $codecSourcePath $ApprovedDllNames

$provenance = Get-Content -Raw -LiteralPath $provenancePath
foreach ($requiredProvenance in @(
    "source_url=$ExpectedSourceUrl",
    "archive_sha256=$ExpectedArchiveSha256",
    "signature=VALID",
    "codec_files=$($ApprovedDllNames -join ',')",
    "codec_imports=VALID"
)) {
    if (-not $provenance.Contains($requiredProvenance)) {
        throw "FFmpeg build provenance 缺少门禁: $requiredProvenance"
    }
}
foreach ($argument in $ExpectedConfigureArguments) {
    if (-not $provenance.Contains("'$argument'")) {
        throw "FFmpeg build provenance 缺少 configure 参数: $argument"
    }
}

$attempt = "$package.stage-$PID-$([Guid]::NewGuid().ToString('N'))"
$backup = "$package.previous-$PID-$([Guid]::NewGuid().ToString('N'))"
$releaseAssetAttempt = "$releaseAsset.stage-$PID-$([Guid]::NewGuid().ToString('N'))"
$releaseAssetBackup = "$releaseAsset.previous-$PID-$([Guid]::NewGuid().ToString('N'))"
Assert-UnderRepoRoot $attempt
Assert-UnderRepoRoot $backup
Assert-UnderRepoRoot $releaseAssetAttempt
Assert-UnderRepoRoot $releaseAssetBackup

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $package), $attempt, $releaseAssetsDirectory | Out-Null
try {
    Copy-Item -LiteralPath $applicationPath -Destination (Join-Path $attempt "freeremotedesk-windows.exe")
    $codecDestination = Join-Path $attempt ($CodecRelativeDirectory.Replace('/', '\'))
    $licenseDestination = Join-Path $attempt "licenses"
    New-Item -ItemType Directory -Force -Path $codecDestination, $licenseDestination | Out-Null
    foreach ($dll in $ApprovedDllNames) {
        Copy-Item -LiteralPath (Join-Path $codecSourcePath $dll) -Destination $codecDestination
    }
    Copy-Item -LiteralPath (Join-Path $licensesSource "FFmpeg-LGPL-2.1-or-later.txt") -Destination $licenseDestination
    Copy-Item -LiteralPath (Join-Path $licensesSource "FFmpeg-NOTICE.txt") -Destination $licenseDestination

    $manifest = Get-Content -Raw -LiteralPath $templatePath | ConvertFrom-Json
    if (-not [string]::IsNullOrWhiteSpace($GitCommit)) {
        if ($GitCommit -cnotmatch '^[0-9a-fA-F]{40}$') {
            throw "GitCommit 必须为完整 40 位十六进制 commit ID"
        }
        $manifest.build.gitCommit = $GitCommit.ToLowerInvariant()
    }
    if (-not [string]::IsNullOrWhiteSpace($BuildId)) {
        if ($BuildId.Length -gt 128 -or $BuildId -cnotmatch '^[A-Za-z0-9._-]+$') {
            throw "BuildId 只能包含字母、数字、点、下划线和连字符，且不得超过 128 字符"
        }
        $manifest.build.buildId = $BuildId
    }
    $manifest.buildProvenanceSha256 = (Get-FileHash -LiteralPath $provenancePath -Algorithm SHA256).Hash
    $correspondingSourceHash = (Get-FileHash -LiteralPath $sourceAssetPath -Algorithm SHA256).Hash
    $manifest.correspondingSource.sha256 = $correspondingSourceHash
    foreach ($entry in @($manifest.files)) {
        $stagedFile = Join-Path $attempt ([string]$entry.path).Replace('/', '\')
        if (-not (Test-Path -LiteralPath $stagedFile -PathType Leaf)) {
            throw "manifest 指向的 staged 文件不存在: $($entry.path)"
        }
        $entry.sha256 = (Get-FileHash -LiteralPath $stagedFile -Algorithm SHA256).Hash
    }
    $manifest.payloadSha256 = Get-PayloadSha256 $manifest.files
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $attempt "ffmpeg-manifest.json") -Encoding UTF8

    try {
        & $verifier -PackageRoot $attempt
    }
    catch {
        throw "临时 Windows package 验证失败"
    }
    Copy-Item -LiteralPath $sourceAssetPath -Destination $releaseAssetAttempt
    if ((Get-FileHash -LiteralPath $releaseAssetAttempt -Algorithm SHA256).Hash -cne $correspondingSourceHash) {
        throw "非隐藏 release staging 的对应源码 ZIP hash 不匹配"
    }

    $hadPackage = Test-Path -LiteralPath $package
    $hadReleaseAsset = Test-Path -LiteralPath $releaseAsset
    $packageBackedUp = $false
    $releaseAssetBackedUp = $false
    $packagePublished = $false
    $releaseAssetPublished = $false
    try {
        if ($hadPackage) {
            Move-Item -LiteralPath $package -Destination $backup
            $packageBackedUp = $true
        }
        if ($hadReleaseAsset) {
            Move-Item -LiteralPath $releaseAsset -Destination $releaseAssetBackup
            $releaseAssetBackedUp = $true
        }
        Move-Item -LiteralPath $attempt -Destination $package
        $packagePublished = $true
        Move-Item -LiteralPath $releaseAssetAttempt -Destination $releaseAsset
        $releaseAssetPublished = $true
        try {
            & $verifier -PackageRoot $package
        }
        catch {
            throw "已发布 Windows package 验证失败"
        }
        if ((Get-FileHash -LiteralPath $releaseAsset -Algorithm SHA256).Hash -cne $correspondingSourceHash) {
            throw "已发布对应源码 ZIP hash 不匹配"
        }
    }
    catch {
        if ($packagePublished -and (Test-Path -LiteralPath $package)) {
            Remove-Item -LiteralPath $package -Recurse -Force
        }
        if ($packageBackedUp -and (Test-Path -LiteralPath $backup)) {
            Move-Item -LiteralPath $backup -Destination $package
        }
        if ($releaseAssetPublished -and (Test-Path -LiteralPath $releaseAsset)) {
            Remove-Item -LiteralPath $releaseAsset -Force
        }
        if ($releaseAssetBackedUp -and (Test-Path -LiteralPath $releaseAssetBackup)) {
            Move-Item -LiteralPath $releaseAssetBackup -Destination $releaseAsset
        }
        throw
    }
    if (Test-Path -LiteralPath $backup) {
        Remove-Item -LiteralPath $backup -Recurse -Force
    }
    if (Test-Path -LiteralPath $releaseAssetBackup) {
        Remove-Item -LiteralPath $releaseAssetBackup -Force
    }
}
finally {
    foreach ($temporary in @($attempt, $backup, $releaseAssetAttempt, $releaseAssetBackup)) {
        if (Test-Path -LiteralPath $temporary) {
            Assert-UnderRepoRoot $temporary
            if (Test-Path -LiteralPath $temporary -PathType Container) {
                Remove-Item -LiteralPath $temporary -Recurse -Force
            }
            else {
                Remove-Item -LiteralPath $temporary -Force
            }
        }
    }
}

Write-Host "Windows package staged: $package"
Write-Host "Corresponding source release staging: $releaseAsset"
