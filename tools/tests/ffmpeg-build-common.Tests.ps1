$modulePath = Join-Path $PSScriptRoot "..\ffmpeg-build-common.psm1"
Import-Module $modulePath -Force

Describe "FFmpeg build security invariants" {
    It "replaces a tampered extracted tree with a fresh hash-verified archive extraction" {
        $archiveInput = Join-Path $TestDrive "archive-input"
        $archiveTree = Join-Path $archiveInput "ffmpeg-8.1.2"
        $destination = Join-Path $TestDrive "source"
        New-Item -ItemType Directory -Force -Path $archiveTree, (Join-Path $destination "ffmpeg-8.1.2") | Out-Null
        Set-Content -LiteralPath (Join-Path $archiveTree "clean.txt") -Value "signed archive content" -Encoding UTF8
        Set-Content -LiteralPath (Join-Path $destination "ffmpeg-8.1.2\tampered.txt") -Value "must disappear" -Encoding UTF8
        $archive = Join-Path $TestDrive "ffmpeg-8.1.2.tar"
        & tar.exe -cf $archive -C $archiveInput "ffmpeg-8.1.2"
        $LASTEXITCODE | Should Be 0
        $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash

        Expand-VerifiedArchiveFresh `
            -Archive $archive `
            -ExpectedArchiveSha256 $archiveHash `
            -DestinationParent $destination `
            -ExpectedDirectoryName "ffmpeg-8.1.2" `
            -AllowedRoot $TestDrive

        (Test-Path -LiteralPath (Join-Path $destination "ffmpeg-8.1.2\tampered.txt")) | Should Be $false
        (Get-Content -Raw -LiteralPath (Join-Path $destination "ffmpeg-8.1.2\clean.txt")).Trim() | Should Be "signed archive content"
    }

    It "replaces a contaminated bundle with exactly the three approved files" {
        $approved = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
        $destination = Join-Path $TestDrive "codec"
        $staging = Join-Path $TestDrive "codec.stage"
        New-Item -ItemType Directory -Force -Path $destination, $staging | Out-Null
        foreach ($name in $approved) {
            Set-Content -LiteralPath (Join-Path $staging $name) -Value $name -Encoding ASCII
        }
        Set-Content -LiteralPath (Join-Path $destination "stale-gpl.dll") -Value "unapproved" -Encoding ASCII

        Publish-ExactDirectory `
            -StagingDirectory $staging `
            -DestinationDirectory $destination `
            -ApprovedFileNames $approved `
            -AllowedRoot $TestDrive

        $actual = @(Get-ChildItem -LiteralPath $destination -File | Sort-Object Name | Select-Object -ExpandProperty Name)
        ($actual -join ",") | Should Be (($approved | Sort-Object) -join ",")
        (Test-Path -LiteralPath (Join-Path $destination "stale-gpl.dll")) | Should Be $false
    }

    It "rejects an unapproved staged bundle without replacing the current bundle" {
        $approved = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
        $destination = Join-Path $TestDrive "current-codec"
        $staging = Join-Path $TestDrive "bad-codec.stage"
        New-Item -ItemType Directory -Force -Path $destination, $staging | Out-Null
        Set-Content -LiteralPath (Join-Path $destination "sentinel.txt") -Value "current bundle" -Encoding ASCII
        foreach ($name in $approved) {
            Set-Content -LiteralPath (Join-Path $staging $name) -Value $name -Encoding ASCII
        }
        Set-Content -LiteralPath (Join-Path $staging "libx265.dll") -Value "forbidden" -Encoding ASCII

        $threw = $false
        try {
            Publish-ExactDirectory `
                -StagingDirectory $staging `
                -DestinationDirectory $destination `
                -ApprovedFileNames $approved `
                -AllowedRoot $TestDrive
        }
        catch {
            $threw = $true
        }

        $threw | Should Be $true
        (Get-Content -Raw -LiteralPath (Join-Path $destination "sentinel.txt")).Trim() | Should Be "current bundle"
    }

    It "stages an exact distributor-controlled corresponding-source package and manifest" {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $inputs = Join-Path $TestDrive "inputs"
        $releaseAssets = Join-Path $TestDrive "release-assets"
        New-Item -ItemType Directory -Force -Path $inputs, $releaseAssets | Out-Null
        $archive = Join-Path $inputs "ffmpeg-8.1.2.tar.xz"
        $signature = "$archive.asc"
        $license = Join-Path $inputs "LICENSE.LGPLv2.1"
        $changes = Join-Path $inputs "changes.diff"
        Set-Content -LiteralPath $archive -Value "exact signed source archive" -Encoding ASCII
        Set-Content -LiteralPath $signature -Value "detached signature" -Encoding ASCII
        Set-Content -LiteralPath $license -Value "LGPL" -Encoding ASCII
        Set-Content -LiteralPath $changes -Value "no changes" -Encoding ASCII
        $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
        $signatureHash = (Get-FileHash -LiteralPath $signature -Algorithm SHA256).Hash

        $package = New-CorrespondingSourcePackage `
            -SourceArchive $archive `
            -ExpectedArchiveSha256 $archiveHash `
            -Signature $signature `
            -ExpectedSignatureSha256 $signatureHash `
            -License $license `
            -Changes $changes `
            -ReleaseAssetsDirectory $releaseAssets `
            -PackageFileName "FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip" `
            -SourceUrl "https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz" `
            -AllowedRoot $TestDrive

        $zip = [IO.Compression.ZipFile]::OpenRead($package)
        try {
            $entries = @($zip.Entries | Sort-Object FullName | Select-Object -ExpandProperty FullName)
            ($entries -join ",") | Should Be "changes.diff,ffmpeg-8.1.2.tar.xz,ffmpeg-8.1.2.tar.xz.asc,LICENSE.LGPLv2.1,SOURCE-MANIFEST.txt"
            $manifestEntry = $zip.GetEntry("SOURCE-MANIFEST.txt")
            $reader = New-Object IO.StreamReader($manifestEntry.Open())
            try {
                $manifest = $reader.ReadToEnd()
            }
            finally {
                $reader.Dispose()
            }
        }
        finally {
            $zip.Dispose()
        }
        $manifest | Should Match ([regex]::Escape("archive_sha256=$archiveHash"))
        $manifest | Should Match "package_file=FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"
        $manifest | Should Match "release_member=ffmpeg-8.1.2.tar.xz"
    }
}
