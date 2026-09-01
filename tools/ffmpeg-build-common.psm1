Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Assert-PathUnderRoot {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    $root = [IO.Path]::GetFullPath($AllowedRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $candidate = [IO.Path]::GetFullPath($Path)
    if (-not $candidate.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝操作允许根目录之外的路径: $candidate"
    }
}

function Assert-ExactDirectoryFiles {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string[]]$ApprovedFileNames
    )

    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        throw "目录不存在: $Directory"
    }
    $children = @(Get-ChildItem -LiteralPath $Directory -Force)
    $directories = @($children | Where-Object { $_.PSIsContainer })
    if ($directories.Count -ne 0) {
        throw "目录包含未批准的子目录: $($directories.Name -join ', ')"
    }
    $actual = @($children | Sort-Object Name | Select-Object -ExpandProperty Name)
    $approved = @($ApprovedFileNames | Sort-Object)
    $actualKey = (($actual | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    $approvedKey = (($approved | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    if ($actualKey -cne $approvedKey) {
        throw "目录文件集合不匹配；实际 [$($actual -join ', ')]，要求 [$($approved -join ', ')]"
    }
}

function Expand-VerifiedArchiveFresh {
    param(
        [Parameter(Mandatory = $true)][string]$Archive,
        [Parameter(Mandatory = $true)][string]$ExpectedArchiveSha256,
        [Parameter(Mandatory = $true)][string]$DestinationParent,
        [Parameter(Mandatory = $true)][string]$ExpectedDirectoryName,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
        throw "源码归档不存在: $Archive"
    }
    $actualHash = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
    if ($actualHash -cne $ExpectedArchiveSha256) {
        throw "解压前源码 SHA-256 不匹配: $actualHash"
    }

    Assert-PathUnderRoot -Path $DestinationParent -AllowedRoot $AllowedRoot
    $suffix = "$PID-$([Guid]::NewGuid().ToString('N'))"
    $staging = "$DestinationParent.extract-$suffix"
    $backup = "$DestinationParent.previous-$suffix"
    Assert-PathUnderRoot -Path $staging -AllowedRoot $AllowedRoot
    Assert-PathUnderRoot -Path $backup -AllowedRoot $AllowedRoot
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $DestinationParent), $staging | Out-Null

    try {
        & tar.exe -xf $Archive -C $staging
        if ($LASTEXITCODE -ne 0) {
            throw "解压 hash-verified 源码归档失败"
        }
        $topLevel = @(Get-ChildItem -LiteralPath $staging -Force)
        if ($topLevel.Count -ne 1 -or -not $topLevel[0].PSIsContainer -or $topLevel[0].Name -cne $ExpectedDirectoryName) {
            throw "源码归档顶层结构不匹配: 期望唯一目录 $ExpectedDirectoryName"
        }

        $hadDestination = Test-Path -LiteralPath $DestinationParent
        if ($hadDestination) {
            Move-Item -LiteralPath $DestinationParent -Destination $backup
        }
        try {
            Move-Item -LiteralPath $staging -Destination $DestinationParent
            $published = Join-Path $DestinationParent $ExpectedDirectoryName
            if (-not (Test-Path -LiteralPath $published -PathType Container)) {
                throw "fresh 源码目录发布后缺失: $published"
            }
        }
        catch {
            if (Test-Path -LiteralPath $DestinationParent) {
                Remove-Item -LiteralPath $DestinationParent -Recurse -Force
            }
            if ($hadDestination -and (Test-Path -LiteralPath $backup)) {
                Move-Item -LiteralPath $backup -Destination $DestinationParent
            }
            throw
        }
        if (Test-Path -LiteralPath $backup) {
            Remove-Item -LiteralPath $backup -Recurse -Force
        }
    }
    finally {
        if (Test-Path -LiteralPath $staging) {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
    }

    return (Join-Path $DestinationParent $ExpectedDirectoryName)
}

function Publish-ExactDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$StagingDirectory,
        [Parameter(Mandatory = $true)][string]$DestinationDirectory,
        [Parameter(Mandatory = $true)][string[]]$ApprovedFileNames,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    Assert-PathUnderRoot -Path $StagingDirectory -AllowedRoot $AllowedRoot
    Assert-PathUnderRoot -Path $DestinationDirectory -AllowedRoot $AllowedRoot
    $stagingParent = [IO.Path]::GetFullPath((Split-Path -Parent $StagingDirectory))
    $destinationParent = [IO.Path]::GetFullPath((Split-Path -Parent $DestinationDirectory))
    if (-not $stagingParent.Equals($destinationParent, [StringComparison]::OrdinalIgnoreCase)) {
        throw "staging 与目标目录必须位于同一父目录，才能安全原子替换"
    }
    Assert-ExactDirectoryFiles -Directory $StagingDirectory -ApprovedFileNames $ApprovedFileNames

    $backup = "$DestinationDirectory.previous-$PID-$([Guid]::NewGuid().ToString('N'))"
    Assert-PathUnderRoot -Path $backup -AllowedRoot $AllowedRoot
    $hadDestination = Test-Path -LiteralPath $DestinationDirectory
    if ($hadDestination) {
        Move-Item -LiteralPath $DestinationDirectory -Destination $backup
    }
    try {
        Move-Item -LiteralPath $StagingDirectory -Destination $DestinationDirectory
        Assert-ExactDirectoryFiles -Directory $DestinationDirectory -ApprovedFileNames $ApprovedFileNames
    }
    catch {
        if (Test-Path -LiteralPath $DestinationDirectory) {
            Remove-Item -LiteralPath $DestinationDirectory -Recurse -Force
        }
        if ($hadDestination -and (Test-Path -LiteralPath $backup)) {
            Move-Item -LiteralPath $backup -Destination $DestinationDirectory
        }
        throw
    }
    if (Test-Path -LiteralPath $backup) {
        Remove-Item -LiteralPath $backup -Recurse -Force
    }
    return $DestinationDirectory
}

function Get-ZipEntrySha256 {
    param([Parameter(Mandatory = $true)]$Entry)

    $stream = $Entry.Open()
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = $sha.ComputeHash($stream)
        return (($bytes | ForEach-Object { $_.ToString("X2") }) -join "")
    }
    finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

function Assert-CorrespondingSourcePackage {
    param(
        [Parameter(Mandatory = $true)][string]$Package,
        [Parameter(Mandatory = $true)][string]$ExpectedArchiveSha256,
        [Parameter(Mandatory = $true)][string]$ExpectedSignatureSha256
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    if (-not (Test-Path -LiteralPath $Package -PathType Leaf)) {
        throw "对应源码 package 不存在: $Package"
    }
    $expectedEntries = @(
        "ffmpeg-8.1.2.tar.xz",
        "ffmpeg-8.1.2.tar.xz.asc",
        "LICENSE.LGPLv2.1",
        "changes.diff",
        "SOURCE-MANIFEST.txt"
    )
    $zip = [IO.Compression.ZipFile]::OpenRead($Package)
    try {
        $actualEntries = @($zip.Entries | Sort-Object FullName | Select-Object -ExpandProperty FullName)
        $actualKey = (($actualEntries | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
        $expectedKey = ((($expectedEntries | Sort-Object) | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
        if ($actualKey -cne $expectedKey) {
            throw "对应源码 package 文件集合不匹配: $($actualEntries -join ', ')"
        }
        $archiveHash = Get-ZipEntrySha256 -Entry $zip.GetEntry("ffmpeg-8.1.2.tar.xz")
        $signatureHash = Get-ZipEntrySha256 -Entry $zip.GetEntry("ffmpeg-8.1.2.tar.xz.asc")
        if ($archiveHash -cne $ExpectedArchiveSha256 -or $signatureHash -cne $ExpectedSignatureSha256) {
            throw "对应源码 package 内嵌 archive/signature hash 不匹配"
        }
    }
    finally {
        $zip.Dispose()
    }
}

function New-CorrespondingSourcePackage {
    param(
        [Parameter(Mandatory = $true)][string]$SourceArchive,
        [Parameter(Mandatory = $true)][string]$ExpectedArchiveSha256,
        [Parameter(Mandatory = $true)][string]$Signature,
        [Parameter(Mandatory = $true)][string]$ExpectedSignatureSha256,
        [Parameter(Mandatory = $true)][string]$License,
        [Parameter(Mandatory = $true)][string]$Changes,
        [Parameter(Mandatory = $true)][string]$ReleaseAssetsDirectory,
        [Parameter(Mandatory = $true)][string]$PackageFileName,
        [Parameter(Mandatory = $true)][string]$SourceUrl,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    $archiveHash = (Get-FileHash -LiteralPath $SourceArchive -Algorithm SHA256).Hash
    $signatureHash = (Get-FileHash -LiteralPath $Signature -Algorithm SHA256).Hash
    if ($archiveHash -cne $ExpectedArchiveSha256 -or $signatureHash -cne $ExpectedSignatureSha256) {
        throw "对应源码 staging 输入 hash 不匹配"
    }
    foreach ($required in @($License, $Changes)) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
            throw "对应源码 staging 输入缺失: $required"
        }
    }

    Assert-PathUnderRoot -Path $ReleaseAssetsDirectory -AllowedRoot $AllowedRoot
    New-Item -ItemType Directory -Force -Path $ReleaseAssetsDirectory | Out-Null
    $suffix = "$PID-$([Guid]::NewGuid().ToString('N'))"
    $staging = Join-Path $ReleaseAssetsDirectory ".corresponding-source.stage-$suffix"
    $package = Join-Path $ReleaseAssetsDirectory $PackageFileName
    $temporaryPackage = "$package.stage-$suffix"
    $backup = "$package.previous-$suffix"
    foreach ($path in @($staging, $package, $temporaryPackage, $backup)) {
        Assert-PathUnderRoot -Path $path -AllowedRoot $AllowedRoot
    }

    New-Item -ItemType Directory -Force -Path $staging | Out-Null
    try {
        Copy-Item -LiteralPath $SourceArchive -Destination (Join-Path $staging "ffmpeg-8.1.2.tar.xz")
        Copy-Item -LiteralPath $Signature -Destination (Join-Path $staging "ffmpeg-8.1.2.tar.xz.asc")
        Copy-Item -LiteralPath $License -Destination (Join-Path $staging "LICENSE.LGPLv2.1")
        Copy-Item -LiteralPath $Changes -Destination (Join-Path $staging "changes.diff")
        @(
            "package_file=$PackageFileName",
            "release_location=sibling asset beside every distributed FreeRemoteDesk Windows binary package",
            "release_member=ffmpeg-8.1.2.tar.xz",
            "source_url=$SourceUrl",
            "archive_sha256=$archiveHash",
            "signature_sha256=$signatureHash",
            "local_changes=changes.diff"
        ) | Set-Content -LiteralPath (Join-Path $staging "SOURCE-MANIFEST.txt") -Encoding UTF8

        Assert-ExactDirectoryFiles -Directory $staging -ApprovedFileNames @(
            "ffmpeg-8.1.2.tar.xz",
            "ffmpeg-8.1.2.tar.xz.asc",
            "LICENSE.LGPLv2.1",
            "changes.diff",
            "SOURCE-MANIFEST.txt"
        )
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [IO.Compression.ZipFile]::CreateFromDirectory(
            $staging,
            $temporaryPackage,
            [IO.Compression.CompressionLevel]::Optimal,
            $false
        )
        Assert-CorrespondingSourcePackage `
            -Package $temporaryPackage `
            -ExpectedArchiveSha256 $ExpectedArchiveSha256 `
            -ExpectedSignatureSha256 $ExpectedSignatureSha256

        $hadPackage = Test-Path -LiteralPath $package
        if ($hadPackage) {
            Move-Item -LiteralPath $package -Destination $backup
        }
        try {
            Move-Item -LiteralPath $temporaryPackage -Destination $package
            Assert-CorrespondingSourcePackage `
                -Package $package `
                -ExpectedArchiveSha256 $ExpectedArchiveSha256 `
                -ExpectedSignatureSha256 $ExpectedSignatureSha256
        }
        catch {
            if (Test-Path -LiteralPath $package) {
                Remove-Item -LiteralPath $package -Force
            }
            if ($hadPackage -and (Test-Path -LiteralPath $backup)) {
                Move-Item -LiteralPath $backup -Destination $package
            }
            throw
        }
        if (Test-Path -LiteralPath $backup) {
            Remove-Item -LiteralPath $backup -Force
        }
    }
    finally {
        if (Test-Path -LiteralPath $staging) {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
        if (Test-Path -LiteralPath $temporaryPackage) {
            Remove-Item -LiteralPath $temporaryPackage -Force
        }
    }

    return $package
}

Export-ModuleMember -Function @(
    "Assert-PathUnderRoot",
    "Assert-ExactDirectoryFiles",
    "Expand-VerifiedArchiveFresh",
    "Publish-ExactDirectory",
    "Assert-CorrespondingSourcePackage",
    "New-CorrespondingSourcePackage"
)
