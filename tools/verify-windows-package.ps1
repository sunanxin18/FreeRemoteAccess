[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot,
    [switch]$RequireTrustedInstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ApprovedCodecDirectory = "codecs/ffmpeg-8.1.2/windows-x86_64"
$ApprovedDllNames = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
$ExpectedSourceUrl = "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz"
$ExpectedSignatureUrl = "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz.asc"
$ExpectedArchiveSha256 = "464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C"
$ExpectedReleaseFingerprint = "FCF986EA15E6E293A5644F10B4322F04D67658D8"
$ExpectedCorrespondingSourceAsset = "FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"
$ExpectedLicenseHashes = @{
    "FFmpeg-LGPL-2.1-or-later.txt" = "246041B6ECF9BC32D718A62C57877C78B5EB397B6467E74ED7AE2626AB189C30"
    "FFmpeg-NOTICE.txt" = "2319907322DA7327C9D6F84B1185C0F71626107F4C81EBC035644DF30ABADB9F"
}
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
    "freeremotedesk-windows.exe",
    "codecs/ffmpeg-8.1.2/windows-x86_64/avcodec-62.dll",
    "codecs/ffmpeg-8.1.2/windows-x86_64/avutil-60.dll",
    "codecs/ffmpeg-8.1.2/windows-x86_64/freeremotedesk_ffmpeg.dll",
    "licenses/FFmpeg-LGPL-2.1-or-later.txt",
    "licenses/FFmpeg-NOTICE.txt"
)
$ExpectedDirectories = @(
    "codecs",
    "codecs/ffmpeg-8.1.2",
    "codecs/ffmpeg-8.1.2/windows-x86_64",
    "licenses"
)
$ExpectedRoles = @{
    "freeremotedesk-windows.exe" = "application"
    "codecs/ffmpeg-8.1.2/windows-x86_64/avcodec-62.dll" = "ffmpeg-libavcodec"
    "codecs/ffmpeg-8.1.2/windows-x86_64/avutil-60.dll" = "ffmpeg-libavutil"
    "codecs/ffmpeg-8.1.2/windows-x86_64/freeremotedesk_ffmpeg.dll" = "freeremotedesk-ffmpeg-plugin"
    "licenses/FFmpeg-LGPL-2.1-or-later.txt" = "license"
    "licenses/FFmpeg-NOTICE.txt" = "notice"
}

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

function Get-SidValue([string]$Identity) {
    try {
        return ([Security.Principal.NTAccount]::new($Identity)).Translate([Security.Principal.SecurityIdentifier]).Value
    }
    catch {
        try {
            return ([Security.Principal.SecurityIdentifier]::new($Identity)).Value
        }
        catch {
            throw "无法解析安全主体 SID: $Identity"
        }
    }
}

function Assert-TrustedObject([string]$Path, [ValidateSet("Ancestor", "Search", "File")][string]$Kind) {
    $item = Get-Item -Force -LiteralPath $Path -ErrorAction Stop
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "受信安装路径检查失败: 路径不得为 reparse point: $Path"
    }
    if ($Kind -eq "File" -and $item.PSIsContainer) {
        throw "受信安装路径检查失败: 需要普通文件: $Path"
    }
    if ($Kind -ne "File" -and -not $item.PSIsContainer) {
        throw "受信安装路径检查失败: 需要目录: $Path"
    }

    $trustedSids = @(
        "S-1-5-18",
        "S-1-5-32-544",
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    )
    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $ownerSid = Get-SidValue ([string]$acl.Owner)
    if ($trustedSids -cnotcontains $ownerSid) {
        throw "受信安装路径检查失败: owner 不受信: $Path ($ownerSid)"
    }
    $sddl = $acl.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
    if ([string]::IsNullOrWhiteSpace($sddl) -or $sddl.Contains("NO_ACCESS_CONTROL")) {
        throw "受信安装路径检查失败: DACL 缺失: $Path"
    }

    [uint64]$dangerous = if ($Kind -eq "Ancestor") {
        0x10000000L -bor 0x00010000L -bor 0x00000040L -bor 0x00040000L -bor 0x00080000L
    }
    else {
        0x10000000L -bor 0x40000000L -bor 0x00010000L -bor 0x00000002L -bor
        0x00000004L -bor 0x00000010L -bor 0x00000100L -bor 0x00040000L -bor 0x00080000L
    }
    if ($Kind -eq "Search") {
        $dangerous = $dangerous -bor 0x00000040L
    }

    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    foreach ($rule in $rules) {
        if (($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) {
            continue
        }
        [uint64]$mask = ([uint64]([int64]$rule.FileSystemRights)) -band 0xFFFFFFFFL
        if (($mask -band $dangerous) -eq 0) {
            continue
        }
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) {
            throw "受信安装路径检查失败: 危险的非 Allow ACE 不受支持: $Path"
        }
        if ($trustedSids -cnotcontains $rule.IdentityReference.Value) {
            throw "受信安装路径检查失败: 普通主体拥有写入或替换权限: $Path ($($rule.IdentityReference.Value))"
        }
    }
}

function Assert-TrustedInstall([string]$Root, [string[]]$PayloadFiles, [string]$CodecDirectory) {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw "受信安装路径检查失败: 仅支持 Windows"
    }
    if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
        throw "受信安装路径检查失败: windows-x86_64 package 必须由 64 位 PowerShell 验证"
    }
    $programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
    $expectedRoot = [IO.Path]::GetFullPath((Join-Path $programFiles "FreeRemoteDesk")).TrimEnd('\')
    $actualRoot = [IO.Path]::GetFullPath($Root).TrimEnd('\')
    if (-not $actualRoot.Equals($expectedRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "受信安装路径检查失败: 安装目录必须为 $expectedRoot"
    }

    $codecRoot = [IO.Path]::GetFullPath((Join-Path $actualRoot $CodecDirectory.Replace('/', '\'))).TrimEnd('\')
    $directories = @()
    $cursor = [IO.DirectoryInfo]::new($codecRoot)
    while ($null -ne $cursor) {
        $directories += $cursor.FullName
        $cursor = $cursor.Parent
    }
    foreach ($directory in $directories) {
        $kind = if ($directory.Equals($actualRoot, [StringComparison]::OrdinalIgnoreCase) -or
            $directory.Equals($codecRoot, [StringComparison]::OrdinalIgnoreCase)) { "Search" } else { "Ancestor" }
        Assert-TrustedObject $directory $kind
    }
    foreach ($relativePath in $PayloadFiles) {
        Assert-TrustedObject (Join-Path $actualRoot $relativePath.Replace('/', '\')) "File"
    }
}

function Assert-ManifestContract($Manifest, [string]$Context) {
    Assert-True ((Get-RequiredProperty $Manifest "schema" $Context) -ceq "freeremotedesk.windows.ffmpeg-package.v1") "$Context schema 不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "ffmpegVersion" $Context) -ceq "8.1.2") "$Context FFmpeg 版本不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "platform" $Context) -ceq "windows-x86_64") "$Context 平台不匹配"
    Assert-True ((Get-RequiredProperty $Manifest "codecDirectory" $Context) -ceq $ApprovedCodecDirectory) "$Context codec 目录不匹配"
    Assert-True ([int](Get-RequiredProperty $Manifest "libavcodecMajor" $Context) -eq 62) "$Context libavcodec major 必须为 62"
    Assert-True ((Get-RequiredProperty $Manifest "payloadHashAlgorithm" $Context) -ceq "sha256-path-nul-sha256-lf-v1") "$Context payload hash 算法不匹配"

    $build = Get-RequiredProperty $Manifest "build" $Context
    $gitCommit = Get-RequiredProperty $build "gitCommit" "$Context build"
    $buildId = Get-RequiredProperty $build "buildId" "$Context build"
    if ($null -ne $gitCommit) {
        Assert-True (([string]$gitCommit) -cmatch '^[0-9a-f]{40}$') "$Context git commit 无效"
    }
    if ($null -ne $buildId) {
        Assert-True (([string]$buildId) -cmatch '^[A-Za-z0-9._-]{1,128}$') "$Context build ID 无效"
    }

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

$package = [IO.Path]::GetFullPath($PackageRoot)
if (-not (Test-Path -LiteralPath $package -PathType Container)) {
    throw "package staging 目录不存在: $package"
}

$manifestPath = Join-Path $package "ffmpeg-manifest.json"
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "staged manifest 不存在: $manifestPath"
}
$manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
Assert-ManifestContract $manifest "staged manifest"
$payloadHash = [string](Get-RequiredProperty $manifest "payloadSha256" "staged manifest")
Assert-True ($payloadHash -cmatch '^[0-9A-F]{64}$') "staged manifest payload SHA-256 无效"
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
    $role = [string](Get-RequiredProperty $entry "role" "staged manifest file")
    Assert-True ($role -ceq $ExpectedRoles[$relativePath]) "manifest role 不匹配: $relativePath"
    $expectedHash = [string](Get-RequiredProperty $entry "sha256" "staged manifest file")
    Assert-True ($expectedHash -cmatch '^[0-9A-F]{64}$') "manifest SHA-256 必须为 64 位大写十六进制: $relativePath"
    $file = Join-Path $package ($relativePath.Replace('/', '\'))
    Assert-True (Test-Path -LiteralPath $file -PathType Leaf) "manifest 文件不存在: $relativePath"
    $actualHash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash
    Assert-True ($actualHash -ceq $expectedHash) "manifest SHA-256 与实际 staged bytes 不匹配: $relativePath"
}
Assert-True ((Get-PayloadSha256 $manifestFiles) -ceq $payloadHash) "staged manifest payload SHA-256 不匹配"

foreach ($licenseName in @("FFmpeg-LGPL-2.1-or-later.txt", "FFmpeg-NOTICE.txt")) {
    $staged = Join-Path $package "licenses\$licenseName"
    Assert-True (Test-Path -LiteralPath $staged -PathType Leaf) "staged 许可/notice 不存在: $staged"
    Assert-True ((Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash -ceq $ExpectedLicenseHashes[$licenseName]) "staged 许可/notice 与固定发布版本不同: $licenseName"
}

$notice = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $package "licenses\FFmpeg-NOTICE.txt")
foreach ($requiredNoticeText in @($ExpectedSourceUrl, $ExpectedCorrespondingSourceAsset, "替换", "重新链接")) {
    Assert-True ($notice.Contains($requiredNoticeText)) "FFmpeg notice 缺少对应源码或替换/重新链接说明: $requiredNoticeText"
}

$approvedDirectoryFull = [IO.Path]::GetFullPath($codecDirectory).TrimEnd('\')
$shadowDlls = @(
    Get-ChildItem -LiteralPath $package -Recurse -File -Filter "*.dll" | Where-Object {
        -not ([IO.Path]::GetFullPath($_.DirectoryName).TrimEnd('\').Equals($approvedDirectoryFull, [StringComparison]::OrdinalIgnoreCase))
    }
)
$shadowDllPaths = @($shadowDlls | ForEach-Object { $_.FullName })
Assert-True ($shadowDlls.Count -eq 0) "package 的版本化 codec 目录之外存在未批准 DLL: $($shadowDllPaths -join ', ')"

$currentDirectoryShadowDlls = @(
    Get-ChildItem -LiteralPath (Get-Location).Path -File -Filter "*.dll"
)
$currentDirectoryShadowDllPaths = @($currentDirectoryShadowDlls | ForEach-Object { $_.FullName })
Assert-True ($currentDirectoryShadowDlls.Count -eq 0) "当前目录存在可优先加载的未批准 DLL: $($currentDirectoryShadowDllPaths -join ', ')"

$packageEntries = @(Get-ChildItem -LiteralPath $package -Recurse -Force)
foreach ($entry in $packageEntries) {
    Assert-True (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) "package 不得包含 reparse 对象: $($entry.FullName)"
}
$packageFiles = @($packageEntries | Where-Object { -not $_.PSIsContainer } | ForEach-Object {
    $prefix = $package.TrimEnd('\') + '\'
    $fullName = [IO.Path]::GetFullPath($_.FullName)
    Assert-True ($fullName.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) "package 文件逃逸根目录: $fullName"
    Get-NormalizedRelativePath $fullName.Substring($prefix.Length)
})
Assert-ExactStringSet $packageFiles @($ExpectedFiles + "ffmpeg-manifest.json") "package 完整文件集合"
$packageDirectories = @($packageEntries | Where-Object { $_.PSIsContainer } | ForEach-Object {
    $prefix = $package.TrimEnd('\') + '\'
    Get-NormalizedRelativePath ([IO.Path]::GetFullPath($_.FullName).Substring($prefix.Length))
})
Assert-ExactStringSet $packageDirectories $ExpectedDirectories "package 完整目录集合"

if ($RequireTrustedInstall) {
    Assert-TrustedInstall $package @($manifestPaths + "ffmpeg-manifest.json") $ApprovedCodecDirectory
}

Write-Host "Windows package verification passed: $package"
Write-Host "Codec directory: $ApprovedCodecDirectory"
foreach ($entry in $manifestFiles) {
    Write-Host "$($entry.path) SHA256=$($entry.sha256)"
}
