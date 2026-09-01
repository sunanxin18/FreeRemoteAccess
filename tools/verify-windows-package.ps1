[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$TemplatePath = Join-Path $RepoRoot "packaging\windows\ffmpeg-manifest.json"
$ApprovedCodecDirectory = "codecs/ffmpeg-8.1.2/windows-x86_64"
$ApprovedDllNames = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
$ExpectedSourceUrl = "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz"
$ExpectedSignatureUrl = "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz.asc"
$ExpectedArchiveSha256 = "464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C"
$ExpectedReleaseFingerprint = "FCF986EA15E6E293A5644F10B4322F04D67658D8"
$ExpectedCorrespondingSourceAsset = "FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"
$ExpectedConfigureArguments = @(
    "--arch=x86_64",
    "--target-os=mingw32",
    "--cross-prefix=x86_64-w64-mingw32-",
    "--disable-static",
    "--enable-shared",
    "--disable-programs",
    "--disable-doc",
    "--disable-everything",
    "--enable-decoder=hevc",
    "--enable-parser=hevc",
    "--enable-protocol=file",
    "--disable-gpl",
    "--disable-nonfree",
    "--disable-version3",
    "--disable-autodetect",
    "--disable-network",
    "--disable-x86asm",
    "--disable-debug",
    "--enable-stripping"
)
$ExpectedFiles = @(
    "codecs/ffmpeg-8.1.2/windows-x86_64/avcodec-62.dll",
    "codecs/ffmpeg-8.1.2/windows-x86_64/avutil-60.dll",
    "codecs/ffmpeg-8.1.2/windows-x86_64/freeremotedesk_ffmpeg.dll",
    "licenses/FFmpeg-LGPL-2.1-or-later.txt",
    "licenses/FFmpeg-NOTICE.txt"
)

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw $Message
    }
}

function Get-RequiredProperty($Value, [string]$Name, [string]$Context) {
    $property = $Value.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "$Context 缺少属性: $Name"
    }
    return $property.Value
}

function Get-NormalizedRelativePath([string]$Path) {
    return $Path.Replace('\', '/')
}

function Assert-ExactStringSet([string[]]$Actual, [string[]]$Expected, [string]$Context) {
    $actualKey = (($Actual | Sort-Object) -join "|")
    $expectedKey = (($Expected | Sort-Object) -join "|")
    if ($actualKey -cne $expectedKey) {
        throw "$Context 不匹配；实际 [$($Actual -join ', ')]，要求 [$($Expected -join ', ')]"
    }
}

function Assert-ManifestContract($Manifest, [string]$Context) {
    Assert-True ((Get-RequiredProperty $Manifest "schema" $Context) -ceq "freeremotedesk.windows.ffmpeg-package.v1") "$Context schema 不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "ffmpegVersion" $Context) -ceq "8.1.2") "$Context FFmpeg 版本不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "platform" $Context) -ceq "windows-x86_64") "$Context 平台不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "codecDirectory" $Context) -ceq $ApprovedCodecDirectory) "$Context codec 目录不匹配"
    Assert-True ([int](Get-RequiredProperty $Manifest "libavcodecMajor" $Context) -eq 62) "$Context libavcodec major 必须为 62"

    $source = Get-RequiredProperty $Manifest "source" $Context
    Assert-True ((Get-RequiredProperty $source "url" "$Context source") -ceq $ExpectedSourceUrl) "$Context source URL 不匹配"
    Assert-True ((Get-RequiredProperty $source "signatureUrl" "$Context source") -ceq $ExpectedSignatureUrl) "$Context signature URL 不匹配"
    Assert-True ((Get-RequiredProperty $source "archiveSha256" "$Context source") -ceq $ExpectedArchiveSha256) "$Context source archive SHA-256 不匹配"
    Assert-True ((Get-RequiredProperty $source "releaseFingerprint" "$Context source") -ceq $ExpectedReleaseFingerprint) "$Context release fingerprint 不匹配"
    $configure = @((Get-RequiredProperty $source "configureArguments" "$Context source") | ForEach-Object { [string]$_ })
    Assert-ExactStringSet $configure $ExpectedConfigureArguments "$Context configure arguments"

    $correspondingSource = Get-RequiredProperty $Manifest "correspondingSource" $Context
    Assert-True ((Get-RequiredProperty $correspondingSource "asset" "$Context correspondingSource") -ceq $ExpectedCorrespondingSourceAsset) "$Context 对应源码 asset 不匹配"
    Assert-True ((Get-RequiredProperty $correspondingSource "distribution" "$Context correspondingSource") -ceq "sibling-release-asset") "$Context 对应源码必须作为 sibling release asset 分发"
    foreach ($name in @("replaceInstructions", "relinkInstructions")) {
        $instructions = [string](Get-RequiredProperty $correspondingSource $name "$Context correspondingSource")
        Assert-True (-not [string]::IsNullOrWhiteSpace($instructions)) "$Context 缺少 $name"
    }
}

if (-not (Test-Path -LiteralPath $TemplatePath -PathType Leaf)) {
    throw "package manifest 模板不存在: $TemplatePath"
}

$template = Get-Content -Raw -LiteralPath $TemplatePath | ConvertFrom-Json
Assert-ManifestContract $template "模板 manifest"

$package = [IO.Path]::GetFullPath($PackageRoot)
if (-not (Test-Path -LiteralPath $package -PathType Container)) {
    throw "package staging 目录不存在: $package"
}

$manifestPath = Join-Path $package "ffmpeg-manifest.json"
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "staged manifest 不存在: $manifestPath"
}
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
Assert-ManifestContract $manifest "staged manifest"
$provenanceHash = [string](Get-RequiredProperty $manifest "buildProvenanceSha256" "staged manifest")
Assert-True ($provenanceHash -cmatch '^[0-9A-F]{64}$') "staged manifest build provenance SHA-256 无效"
$correspondingSource = Get-RequiredProperty $manifest "correspondingSource" "staged manifest"
$correspondingSourceHash = [string](Get-RequiredProperty $correspondingSource "sha256" "staged manifest correspondingSource")
Assert-True ($correspondingSourceHash -cmatch '^[0-9A-F]{64}$') "staged manifest 对应源码 SHA-256 无效"

$application = Join-Path $package "freeremotedesk-windows.exe"
Assert-True (Test-Path -LiteralPath $application -PathType Leaf) "Windows GUI executable 不存在: $application"

$codecDirectory = Join-Path $package ($ApprovedCodecDirectory.Replace('/', '\'))
Assert-True (Test-Path -LiteralPath $codecDirectory -PathType Container) "版本化 codec 目录不存在: $codecDirectory"
$codecChildren = @(Get-ChildItem -LiteralPath $codecDirectory -Force)
Assert-True (@($codecChildren | Where-Object { $_.PSIsContainer }).Count -eq 0) "codec 目录不得包含子目录"
Assert-ExactStringSet @($codecChildren | Select-Object -ExpandProperty Name) $ApprovedDllNames "codec 目录文件集合"

$manifestFiles = @(Get-RequiredProperty $manifest "files" "staged manifest")
$manifestPaths = @($manifestFiles | ForEach-Object { Get-NormalizedRelativePath ([string](Get-RequiredProperty $_ "path" "staged manifest file")) })
Assert-ExactStringSet $manifestPaths $ExpectedFiles "manifest 文件集合"
foreach ($entry in $manifestFiles) {
    $relativePath = Get-NormalizedRelativePath ([string](Get-RequiredProperty $entry "path" "staged manifest file"))
    $expectedHash = [string](Get-RequiredProperty $entry "sha256" "staged manifest file")
    Assert-True ($expectedHash -cmatch '^[0-9A-F]{64}$') "manifest SHA-256 必须为 64 位大写十六进制: $relativePath"
    $file = Join-Path $package ($relativePath.Replace('/', '\'))
    Assert-True (Test-Path -LiteralPath $file -PathType Leaf) "manifest 文件不存在: $relativePath"
    $actualHash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash
    Assert-True ($actualHash -ceq $expectedHash) "manifest SHA-256 与实际 staged bytes 不匹配: $relativePath"
}

$templateFiles = @((Get-RequiredProperty $template "files" "模板 manifest") | ForEach-Object { Get-NormalizedRelativePath ([string](Get-RequiredProperty $_ "path" "模板 manifest file")) })
Assert-ExactStringSet $templateFiles $ExpectedFiles "模板 manifest 文件集合"

foreach ($licenseName in @("FFmpeg-LGPL-2.1-or-later.txt", "FFmpeg-NOTICE.txt")) {
    $committed = Join-Path $RepoRoot "packaging\windows\licenses\$licenseName"
    $staged = Join-Path $package "licenses\$licenseName"
    Assert-True (Test-Path -LiteralPath $committed -PathType Leaf) "提交的许可/notice 不存在: $committed"
    Assert-True (Test-Path -LiteralPath $staged -PathType Leaf) "staged 许可/notice 不存在: $staged"
    Assert-True ((Get-FileHash -LiteralPath $committed -Algorithm SHA256).Hash -ceq (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash) "staged 许可/notice 与提交版本不同: $licenseName"
}

$notice = Get-Content -Raw -LiteralPath (Join-Path $package "licenses\FFmpeg-NOTICE.txt")
foreach ($requiredNoticeText in @($ExpectedSourceUrl, $ExpectedCorrespondingSourceAsset, "替换", "重新链接")) {
    Assert-True ($notice.Contains($requiredNoticeText)) "FFmpeg notice 缺少对应源码或替换/重新链接说明: $requiredNoticeText"
}

$approvedDirectoryFull = [IO.Path]::GetFullPath($codecDirectory).TrimEnd('\')
$shadowDlls = @(
    Get-ChildItem -LiteralPath $package -Recurse -File -Filter "*.dll" | Where-Object {
        $_.Name -in $ApprovedDllNames -and
        -not ([IO.Path]::GetFullPath($_.DirectoryName).TrimEnd('\').Equals($approvedDirectoryFull, [StringComparison]::OrdinalIgnoreCase))
    }
)
$shadowDllPaths = @($shadowDlls | ForEach-Object { $_.FullName })
Assert-True ($shadowDlls.Count -eq 0) "package 中存在可遮蔽 approved bundle 的 DLL: $($shadowDllPaths -join ', ')"

$currentDirectoryShadowDlls = @(
    Get-ChildItem -LiteralPath (Get-Location).Path -File -Filter "*.dll" | Where-Object { $_.Name -in $ApprovedDllNames }
)
$currentDirectoryShadowDllPaths = @($currentDirectoryShadowDlls | ForEach-Object { $_.FullName })
Assert-True ($currentDirectoryShadowDlls.Count -eq 0) "当前目录存在可优先加载的 approved DLL: $($currentDirectoryShadowDllPaths -join ', ')"

Write-Host "Windows package verification passed: $package"
Write-Host "Codec directory: $ApprovedCodecDirectory"
foreach ($entry in $manifestFiles) {
    Write-Host "$($entry.path) SHA256=$($entry.sha256)"
}
