$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$packageRoot = Join-Path $repoRoot "target\package-test"
$codecRoot = Join-Path $packageRoot "codecs\ffmpeg-8.1.2\windows-x86_64"
$verifier = Join-Path $repoRoot "tools\verify-windows-package.ps1"

Describe "Windows package verification security boundaries" {
    BeforeAll {
        if (-not (Test-Path -LiteralPath $packageRoot -PathType Container)) {
            throw "先运行 tools/stage-windows-package.ps1 生成测试 package"
        }
    }

    It "accepts the exact staged package" {
        $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
        $LASTEXITCODE | Should Be 0
        ($output -join "`n") | Should Match "Windows package verification passed"
    }

    It "rejects an approved DLL shadow copy in the package root" {
        $shadow = Join-Path $packageRoot "avcodec-62.dll"
        Copy-Item -LiteralPath (Join-Path $codecRoot "avcodec-62.dll") -Destination $shadow
        try {
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "可遮蔽 approved bundle"
        }
        finally {
            Remove-Item -LiteralPath $shadow -Force
        }
    }

    It "rejects an approved DLL shadow copy in the current directory" {
        $currentDirectory = Join-Path $TestDrive "shadow-current-directory"
        New-Item -ItemType Directory -Force -Path $currentDirectory | Out-Null
        Copy-Item -LiteralPath (Join-Path $codecRoot "avutil-60.dll") -Destination $currentDirectory
        Push-Location $currentDirectory
        try {
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "当前目录存在可优先加载"
        }
        finally {
            Pop-Location
        }
    }

    It "rejects a staged compliance file whose bytes no longer match the manifest" {
        $notice = Join-Path $packageRoot "licenses\FFmpeg-NOTICE.txt"
        $backup = Join-Path $TestDrive "FFmpeg-NOTICE.original.txt"
        Copy-Item -LiteralPath $notice -Destination $backup
        try {
            Add-Content -LiteralPath $notice -Value "tampered" -Encoding UTF8
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "实际 staged bytes 不匹配"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $notice -Force
        }
    }
}
