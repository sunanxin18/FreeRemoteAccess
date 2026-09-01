[CmdletBinding()]
param(
    [ValidateSet("Release", "Debug")]
    [string]$Configuration = "Release",
    [string]$WslDistribution = "ubuntu24.04",
    [switch]$ForceRebuild,
    [switch]$RunNativeTests
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Version = "8.1.2"
$ArchiveSha256 = "464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C"
$ReleaseFingerprint = "FCF986EA15E6E293A5644F10B4322F04D67658D8"
$SourceUrl = "https://ffmpeg.org/releases/ffmpeg-$Version.tar.xz"
$SignatureUrl = "$SourceUrl.asc"
$KeyUrl = "https://ffmpeg.org/ffmpeg-devel.asc"
$ApprovedBundleFiles = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
$CorrespondingSourcePackageName = "FreeRemoteDesk-ffmpeg-$Version-corresponding-source.zip"

$RepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
Import-Module (Join-Path $PSScriptRoot "ffmpeg-build-common.psm1") -Force
$BuildRoot = Join-Path $RepoRoot ".codex-target\ffmpeg-$Version"
$SourceCache = Join-Path $BuildRoot "source-cache"
$GpgHome = Join-Path $BuildRoot "gnupg"
$ConfigurationRoot = Join-Path $BuildRoot "windows-x86_64\$Configuration"
$CodecDir = Join-Path $ConfigurationRoot "codec"
$ReleaseAssetsDirectory = Join-Path $BuildRoot "release-assets"
$Archive = Join-Path $SourceCache "ffmpeg-$Version.tar.xz"
$Signature = "$Archive.asc"
$SigningKey = Join-Path $SourceCache "ffmpeg-devel.asc"
$ProvenanceLog = Join-Path $ConfigurationRoot "build-provenance.txt"
$ThirdPartyRoot = Join-Path $RepoRoot "third_party\ffmpeg\$Version"
$AttemptId = "$PID-$([Guid]::NewGuid().ToString('N'))"
$AttemptRoot = Join-Path $ConfigurationRoot ".attempt-$AttemptId"
$SourceParent = Join-Path $AttemptRoot "source"
$SourceDir = Join-Path $SourceParent "ffmpeg-$Version"
$DistDir = Join-Path $AttemptRoot "dist"
$CodecStage = Join-Path $ConfigurationRoot ".codec.stage-$AttemptId"
$ConfigureRecord = Join-Path $AttemptRoot "configure-command.txt"
$BuildLockPath = Join-Path $BuildRoot "windows-x86_64-$Configuration.lock"

function Assert-UnderBuildRoot([string]$Path) {
    Assert-PathUnderRoot -Path $Path -AllowedRoot $BuildRoot
}

function Download-IfMissing([string]$Url, [string]$Destination) {
    if (Test-Path -LiteralPath $Destination) {
        return
    }
    $parent = Split-Path -Parent $Destination
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $temporary = "$Destination.download"
    Assert-UnderBuildRoot $temporary
    & curl.exe --fail --location --proto '=https' --tlsv1.2 --output $temporary $Url
    if ($LASTEXITCODE -ne 0) {
        throw "下载失败: $Url"
    }
    Move-Item -LiteralPath $temporary -Destination $Destination
}

function Find-Gpg {
    $command = Get-Command gpg.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    $gitGpg = "C:\Program Files\Git\usr\bin\gpg.exe"
    if (Test-Path -LiteralPath $gitGpg) {
        return $gitGpg
    }
    throw "未找到 GPG；无法验证 FFmpeg 发布签名"
}

function Convert-ToWslPath([string]$Path) {
    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    $escaped = $Path.Replace("'", $singleQuoteEscape)
    $result = & wsl.exe --distribution $WslDistribution -- bash -lc "wslpath -a '$escaped'" 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($result)) {
        throw "无法转换 WSL 路径: $Path"
    }
    return $result.Trim()
}

function Invoke-Wsl([string]$WorkingDirectory, [string]$Command, [string]$Action) {
    & wsl.exe --distribution $WslDistribution --cd $WorkingDirectory -- bash -lc $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Action 失败"
    }
}

function Quote-Bash([string]$Value) {
    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $singleQuoteEscape) + "'"
}

function Find-VisualStudioTool([string]$Pattern) {
    $vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw "未找到 vswhere.exe；需要 Visual Studio 2022 C++ 工具链"
    }
    $tool = & $vswhere -latest -products * -find $Pattern | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($tool)) {
        throw "Visual Studio 中缺少工具: $Pattern"
    }
    return $tool
}

function Import-VisualStudioEnvironment([string]$DevCmd) {
    $commandLine = "`"$DevCmd`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
    $lines = & cmd.exe /d /s /c $commandLine
    if ($LASTEXITCODE -ne 0) {
        throw "无法初始化 Visual Studio x64 环境"
    }
    foreach ($line in $lines) {
        $separator = $line.IndexOf('=')
        if ($separator -le 0) {
            continue
        }
        $name = $line.Substring(0, $separator)
        $value = $line.Substring($separator + 1)
        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
}

function Get-PeImports([string]$Dumpbin, [string]$File) {
    $output = & $Dumpbin /nologo /dependents $File
    if ($LASTEXITCODE -ne 0) {
        throw "无法检查 PE imports: $File"
    }
    $imports = @(
        $output | ForEach-Object {
            if ($_ -match '^\s+([A-Za-z0-9_.-]+\.dll)\s*$') {
                $Matches[1]
            }
        }
    )
    if ($imports.Count -eq 0) {
        throw "PE import 集合为空或无法解析: $File"
    }
    return $imports
}

function Assert-ExactPeImports(
    [string]$Dumpbin,
    [string]$File,
    [string[]]$ExpectedImports
) {
    $actual = @(Get-PeImports -Dumpbin $Dumpbin -File $File | Sort-Object)
    $expected = @($ExpectedImports | Sort-Object)
    $actualKey = (($actual | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    $expectedKey = (($expected | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    if ($actualKey -cne $expectedKey) {
        throw "PE imports 不匹配: $File；实际 [$($actual -join ', ')]，要求 [$($expected -join ', ')]"
    }
}

function Assert-ApprovedBundleImports([string]$Dumpbin, [string]$BundleDirectory) {
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "freeremotedesk_ffmpeg.dll") -ExpectedImports @(
        "avcodec-62.dll",
        "avutil-60.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
        "kernel32.dll",
        "ntdll.dll",
        "VCRUNTIME140.dll",
        "api-ms-win-crt-heap-l1-1-0.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll"
    )
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "avcodec-62.dll") -ExpectedImports @(
        "KERNEL32.dll",
        "msvcrt.dll",
        "avutil-60.dll"
    )
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "avutil-60.dll") -ExpectedImports @(
        "bcrypt.dll",
        "KERNEL32.dll",
        "msvcrt.dll"
    )
}

New-Item -ItemType Directory -Force -Path $BuildRoot, $SourceCache, $GpgHome | Out-Null
$buildLock = $null
Push-Location $RepoRoot
try {
    try {
        $buildLock = [IO.File]::Open(
            $BuildLockPath,
            [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::None
        )
    }
    catch {
        throw "同一 configuration 的 FFmpeg build 已在运行: $BuildLockPath"
    }

    Download-IfMissing $SourceUrl $Archive
    Download-IfMissing $SignatureUrl $Signature
    Download-IfMissing $KeyUrl $SigningKey

    $actualHash = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
    if ($actualHash -cne $ArchiveSha256) {
        throw "FFmpeg 源码 SHA-256 不匹配: $actualHash"
    }
    $signatureHash = (Get-FileHash -LiteralPath $Signature -Algorithm SHA256).Hash
    $signingKeyHash = (Get-FileHash -LiteralPath $SigningKey -Algorithm SHA256).Hash

    $gpg = Find-Gpg
    $gpgRelative = [IO.Path]::GetRelativePath($RepoRoot, $GpgHome).Replace('\', '/')
    $oldGpgHome = $env:GNUPGHOME
    $env:GNUPGHOME = $gpgRelative
    try {
        # 公钥验证无需 agent；精确 fingerprint 与 VALIDSIG 是不可省略的成功门禁。
        & $gpg --batch --no-autostart --import $SigningKey 2>&1 | Out-Host
        $fingerprints = (& $gpg --batch --no-autostart --with-colons --fingerprint "FFmpeg release signing key") -join "`n"
        if ($fingerprints -notmatch "fpr:::::::::${ReleaseFingerprint}:") {
            throw "FFmpeg 发布密钥指纹不匹配"
        }
        $signatureStatus = (& $gpg --batch --no-autostart --status-fd 1 --verify $Signature $Archive 2>&1) -join "`n"
        if ($LASTEXITCODE -ne 0 -or $signatureStatus -notmatch "VALIDSIG $ReleaseFingerprint ") {
            throw "FFmpeg 8.1.2 发布签名验证失败`n$signatureStatus"
        }
    }
    finally {
        $env:GNUPGHOME = $oldGpgHome
    }

    New-Item -ItemType Directory -Force -Path $ConfigurationRoot, $AttemptRoot | Out-Null
    # 旧版脚本留下的 source/dist/configure cache 从不再使用；每次 build 都从签名归档 fresh 解压。
    foreach ($legacy in @(
        (Join-Path $ConfigurationRoot "source"),
        (Join-Path $ConfigurationRoot "dist"),
        (Join-Path $ConfigurationRoot "configure-command.txt")
    )) {
        if (Test-Path -LiteralPath $legacy) {
            Assert-UnderBuildRoot $legacy
            Remove-Item -LiteralPath $legacy -Recurse -Force
        }
    }
    if ($ForceRebuild) {
        Get-ChildItem -LiteralPath $ConfigurationRoot -Force | Where-Object {
            $_.Name -like '.attempt-*' -or $_.Name -like '.codec.stage-*' -or $_.Name -like '*.previous-*'
        } | Where-Object { $_.FullName -ne $AttemptRoot } | ForEach-Object {
            Assert-UnderBuildRoot $_.FullName
            Remove-Item -LiteralPath $_.FullName -Recurse -Force
        }
    }

    $SourceDir = Expand-VerifiedArchiveFresh `
        -Archive $Archive `
        -ExpectedArchiveSha256 $ArchiveSha256 `
        -DestinationParent $SourceParent `
        -ExpectedDirectoryName "ffmpeg-$Version" `
        -AllowedRoot $BuildRoot

    & wsl.exe --distribution $WslDistribution -- bash -lc 'command -v bash >/dev/null && command -v make >/dev/null && command -v x86_64-w64-mingw32-gcc >/dev/null'
    if ($LASTEXITCODE -ne 0) {
        throw "WSL distribution '$WslDistribution' 缺少 bash/make/x86_64-w64-mingw32-gcc"
    }
    $sourceWsl = Convert-ToWslPath $SourceDir
    $distWsl = Convert-ToWslPath $DistDir
    $configureArgs = @(
        "--prefix=$distWsl",
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
        "--disable-x86asm"
    )
    if ($Configuration -eq "Release") {
        $configureArgs += "--disable-debug", "--enable-stripping"
    }
    else {
        $configureArgs += "--enable-debug", "--disable-stripping"
    }
    $configureCommand = "./configure " + (($configureArgs | ForEach-Object { Quote-Bash $_ }) -join " ")
    Invoke-Wsl $sourceWsl $configureCommand "FFmpeg configure"
    Set-Content -LiteralPath $ConfigureRecord -Value $configureCommand -Encoding UTF8
    Invoke-Wsl $sourceWsl "make -j4" "FFmpeg build"
    Invoke-Wsl $sourceWsl "make install" "FFmpeg install"

    $configHeader = @(
        Get-Content -Raw -LiteralPath (Join-Path $SourceDir "config.h")
        Get-Content -Raw -LiteralPath (Join-Path $SourceDir "config_components.h")
    ) -join "`n"
    foreach ($required in @(
        "#define CONFIG_GPL 0",
        "#define CONFIG_NONFREE 0",
        "#define CONFIG_HEVC_DECODER 1",
        "#define CONFIG_HEVC_PARSER 1",
        "#define CONFIG_FILE_PROTOCOL 1"
    )) {
        if (-not $configHeader.Contains($required)) {
            throw "FFmpeg 配置缺少必需门禁: $required"
        }
    }

    $libExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\x64\lib.exe"
    $clExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\x64\cl.exe"
    $dumpbinExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe"
    foreach ($library in @(
        @{ Name = "avutil"; Definition = "avutil-60.def" },
        @{ Name = "avcodec"; Definition = "avcodec-62.def" }
    )) {
        $definition = Join-Path $DistDir "lib\$($library.Definition)"
        $outputLibrary = Join-Path $DistDir "lib\$($library.Name).lib"
        & $libExe /nologo "/def:$definition" /machine:x64 "/out:$outputLibrary"
        if ($LASTEXITCODE -ne 0) {
            throw "生成 MSVC import library 失败: $($library.Name)"
        }
    }

    $devCmd = Find-VisualStudioTool "Common7\Tools\VsDevCmd.bat"
    Import-VisualStudioEnvironment $devCmd
    $env:FFMPEG_DIR = $DistDir
    if ($Configuration -eq "Release") {
        & cargo.exe build --locked -p frd-video-ffmpeg-plugin --features native-ffmpeg --release
        $plugin = Join-Path $RepoRoot "target\release\freeremotedesk_ffmpeg.dll"
    }
    else {
        & cargo.exe build --locked -p frd-video-ffmpeg-plugin --features native-ffmpeg
        $plugin = Join-Path $RepoRoot "target\debug\freeremotedesk_ffmpeg.dll"
    }
    if ($LASTEXITCODE -ne 0) {
        throw "native FFmpeg plugin 构建失败"
    }

    New-Item -ItemType Directory -Force -Path $CodecStage | Out-Null
    Copy-Item -LiteralPath (Join-Path $DistDir "bin\avutil-60.dll") -Destination $CodecStage
    Copy-Item -LiteralPath (Join-Path $DistDir "bin\avcodec-62.dll") -Destination $CodecStage
    Copy-Item -LiteralPath $plugin -Destination $CodecStage
    Assert-ExactDirectoryFiles -Directory $CodecStage -ApprovedFileNames $ApprovedBundleFiles
    Assert-ApprovedBundleImports -Dumpbin $dumpbinExe -BundleDirectory $CodecStage

    $correspondingSourcePackage = New-CorrespondingSourcePackage `
        -SourceArchive $Archive `
        -ExpectedArchiveSha256 $ArchiveSha256 `
        -Signature $Signature `
        -ExpectedSignatureSha256 $signatureHash `
        -License (Join-Path $ThirdPartyRoot "LICENSE.LGPLv2.1") `
        -Changes (Join-Path $ThirdPartyRoot "changes.diff") `
        -ReleaseAssetsDirectory $ReleaseAssetsDirectory `
        -PackageFileName $CorrespondingSourcePackageName `
        -SourceUrl $SourceUrl `
        -AllowedRoot $BuildRoot

    Publish-ExactDirectory `
        -StagingDirectory $CodecStage `
        -DestinationDirectory $CodecDir `
        -ApprovedFileNames $ApprovedBundleFiles `
        -AllowedRoot $BuildRoot | Out-Null
    Assert-ExactDirectoryFiles -Directory $CodecDir -ApprovedFileNames $ApprovedBundleFiles
    Assert-ApprovedBundleImports -Dumpbin $dumpbinExe -BundleDirectory $CodecDir

    $toolProvenance = & wsl.exe --distribution $WslDistribution -- bash -lc 'printf "bash="; bash --version | head -1; printf "make="; make --version | head -1; printf "cc="; x86_64-w64-mingw32-gcc --version | head -1'
    $provenanceStage = "$ProvenanceLog.stage-$AttemptId"
    Assert-UnderBuildRoot $provenanceStage
    @(
        "source_url=$SourceUrl",
        "signature_url=$SignatureUrl",
        "signing_key_url=$KeyUrl",
        "archive_sha256=$actualHash",
        "signature_sha256=$signatureHash",
        "signing_key_sha256=$signingKeyHash",
        "release_fingerprint=$ReleaseFingerprint",
        "signature=VALID",
        "source_extraction=fresh-per-build",
        "wsl_distribution=$WslDistribution",
        "msvc_cl=$clExe",
        "msvc_cl_version=$((Get-Item -LiteralPath $clExe).VersionInfo.FileVersion)",
        "configuration=$Configuration",
        "configure=$configureCommand",
        "codec_files=$($ApprovedBundleFiles -join ',')",
        "codec_imports=VALID",
        "corresponding_source_package=$correspondingSourcePackage",
        "corresponding_source_package_sha256=$((Get-FileHash -LiteralPath $correspondingSourcePackage -Algorithm SHA256).Hash)",
        $toolProvenance
    ) | Set-Content -LiteralPath $provenanceStage -Encoding UTF8
    Move-Item -Force -LiteralPath $provenanceStage -Destination $ProvenanceLog

    if ($RunNativeTests) {
        $oldPath = $env:PATH
        $env:PATH = "$CodecDir;$($env:PATH)"
        try {
            & cargo.exe test --locked -p frd-video-ffmpeg-plugin --features native-ffmpeg --lib -- --nocapture
            if ($LASTEXITCODE -ne 0) {
                throw "native FFmpeg plugin 单元测试失败"
            }
            & cargo.exe test --locked -p frd-video-ffmpeg-plugin --test main444_decode -- --nocapture
            if ($LASTEXITCODE -ne 0) {
                throw "Main444 fixture integration test 失败"
            }
        }
        finally {
            $env:PATH = $oldPath
        }
    }

    Write-Host "FFmpeg $Version LGPL shared build complete from a fresh verified extraction."
    Write-Host "Codec bundle: $CodecDir"
    foreach ($file in Get-ChildItem -LiteralPath $CodecDir -File | Sort-Object Name) {
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
        Write-Host "$($file.Name) $($file.Length) bytes SHA256=$hash"
    }
    Write-Host "Corresponding-source release asset: $correspondingSourcePackage"
}
finally {
    try {
        foreach ($temporary in @($AttemptRoot, $CodecStage)) {
            if (Test-Path -LiteralPath $temporary) {
                Assert-UnderBuildRoot $temporary
                Remove-Item -LiteralPath $temporary -Recurse -Force
            }
        }
    }
    finally {
        if ($null -ne $buildLock) {
            $buildLock.Dispose()
        }
        Pop-Location
    }
}
