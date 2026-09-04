$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$packageRoot = Join-Path $repoRoot "target\package-test"
$codecRoot = Join-Path $packageRoot "codecs\ffmpeg-8.1.2\windows-x86_64"
$stager = Join-Path $repoRoot "tools\stage-windows-package.ps1"
$verifier = Join-Path $repoRoot "tools\verify-windows-package.ps1"
$installer = Join-Path $repoRoot "tools\install-windows-package.ps1"
$bootstrapBuilder = Join-Path $repoRoot "tools\new-windows-installer-bootstrap.ps1"
$correspondingSourceAsset = Join-Path $repoRoot "target\release-assets\FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"
$buildProvenance = Join-Path $repoRoot ".codex-target\ffmpeg-8.1.2\windows-x86_64\Release\build-provenance.txt"
$manifestTemplate = Join-Path $repoRoot "packaging\windows\ffmpeg-manifest.json"
$configureRecord = Join-Path $repoRoot "third_party\ffmpeg\8.1.2\configure-windows.txt"
$expectedX86AsmProvenance = "nasm=NASM version 2.16.01"
$expectedHaveX86Asm = "have_x86asm=1"

function Copy-Utf8ScriptForWindowsPowerShell([string]$Source, [string]$Destination) {
    $bom = [Text.Encoding]::UTF8.GetPreamble()
    $sourceBytes = [IO.File]::ReadAllBytes($Source)
    $hasBom = $sourceBytes.Length -ge 3 -and
        $sourceBytes[0] -eq 0xEF -and
        $sourceBytes[1] -eq 0xBB -and
        $sourceBytes[2] -eq 0xBF
    $windowsPowerShellBytes = if ($hasBom) {
        $sourceBytes
    }
    else {
        [byte[]]@($bom + $sourceBytes)
    }
    [IO.File]::WriteAllBytes($Destination, $windowsPowerShellBytes)
}

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

    It "stages only x86asm-enabled FFmpeg provenance and records positive evidence" {
        $template = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestTemplate | ConvertFrom-Json
        (@($template.source.configureArguments) -ccontains "--disable-x86asm") | Should Be $false
        ((Get-Content -Raw -LiteralPath $configureRecord).Contains("--disable-x86asm")) | Should Be $false

        $stageRoot = "target/package-test-x86asm-$PID"
        $stagePath = Join-Path $repoRoot $stageRoot
        try {
            $output = & pwsh -NoProfile -File $stager -PackageRoot $stageRoot 2>&1
            $LASTEXITCODE | Should Be 0
            ($output -join "`n") | Should Match "Windows package staged"

            $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $stagePath "ffmpeg-manifest.json") | ConvertFrom-Json
            (@($manifest.source.configureArguments) -ccontains "--disable-x86asm") | Should Be $false
            $manifest.source.x86asmProvenance | Should Be $expectedX86AsmProvenance
            $manifest.source.haveX86Asm | Should Be $expectedHaveX86Asm

            $verification = & pwsh -NoProfile -File $verifier -PackageRoot $stagePath 2>&1
            $LASTEXITCODE | Should Be 0
            ($verification -join "`n") | Should Match "Windows package verification passed"
        }
        finally {
            if (Test-Path -LiteralPath $stagePath) {
                Remove-Item -LiteralPath $stagePath -Recurse -Force
            }
        }
    }

    It "rejects provenance that disables x86asm before staging" {
        $tamperedProvenance = Join-Path $TestDrive "build-provenance-disabled-x86asm.txt"
        Copy-Item -LiteralPath $buildProvenance -Destination $tamperedProvenance
        Add-Content -LiteralPath $tamperedProvenance -Value "configure=./configure '--disable-x86asm'" -Encoding UTF8

        $output = & pwsh -NoProfile -File $stager `
            -PackageRoot "target/package-test-disabled-x86asm-$PID" `
            -BuildProvenance $tamperedProvenance 2>&1

        $LASTEXITCODE | Should Not Be 0
        ($output -join "`n") | Should Match "不得包含 configure 参数: --disable-x86asm"
    }

    It "rejects NASM-present provenance when generated headers disabled x86asm" {
        $tamperedProvenance = Join-Path $TestDrive "build-provenance-have-x86asm-zero.txt"
        $provenanceLines = @(Get-Content -LiteralPath $buildProvenance | Where-Object { $_ -cne $expectedHaveX86Asm })
        $provenanceLines + "have_x86asm=0" | Set-Content -LiteralPath $tamperedProvenance -Encoding UTF8

        $output = & pwsh -NoProfile -File $stager `
            -PackageRoot "target/package-test-have-x86asm-zero-$PID" `
            -BuildProvenance $tamperedProvenance 2>&1

        $LASTEXITCODE | Should Not Be 0
        ($output -join "`n") | Should Match "have_x86asm=1"
    }

    It "rejects a manifest that reintroduces disabled x86asm" {
        $manifestPath = Join-Path $packageRoot "ffmpeg-manifest.json"
        $backup = Join-Path $TestDrive "ffmpeg-manifest.disabled-x86asm.json"
        Copy-Item -LiteralPath $manifestPath -Destination $backup
        try {
            $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
            $manifest.source.configureArguments += "--disable-x86asm"
            $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "configure arguments 不匹配"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $manifestPath -Force
        }
    }

    It "rejects a manifest without positive x86asm provenance" {
        $manifestPath = Join-Path $packageRoot "ffmpeg-manifest.json"
        $backup = Join-Path $TestDrive "ffmpeg-manifest.x86asm-provenance.json"
        Copy-Item -LiteralPath $manifestPath -Destination $backup
        try {
            $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
            $manifest.source.PSObject.Properties.Remove("x86asmProvenance")
            $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "x86asm positive provenance"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $manifestPath -Force
        }
    }

    It "rejects a manifest without generated-header x86asm evidence" {
        $manifestPath = Join-Path $packageRoot "ffmpeg-manifest.json"
        $backup = Join-Path $TestDrive "ffmpeg-manifest.have-x86asm.json"
        Copy-Item -LiteralPath $manifestPath -Destination $backup
        try {
            $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
            $manifest.source.PSObject.Properties.Remove("haveX86Asm")
            $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "生成头 x86asm 门禁"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $manifestPath -Force
        }
    }

    It "rejects a manifest whose generated-header x86asm evidence is tampered" {
        $manifestPath = Join-Path $packageRoot "ffmpeg-manifest.json"
        $backup = Join-Path $TestDrive "ffmpeg-manifest.have-x86asm-tampered.json"
        Copy-Item -LiteralPath $manifestPath -Destination $backup
        try {
            $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
            $manifest.source | Add-Member -NotePropertyName "haveX86Asm" -NotePropertyValue "have_x86asm=0" -Force
            $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "生成头 x86asm 门禁"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $manifestPath -Force
        }
    }

    It "binds the Windows GUI executable bytes into the complete payload manifest" {
        $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $packageRoot "ffmpeg-manifest.json") | ConvertFrom-Json
        $applicationEntries = @($manifest.files | Where-Object { $_.path -ceq "freeremotedesk-windows.exe" -and $_.role -ceq "application" })

        $applicationEntries.Count | Should Be 1
        $applicationEntries[0].sha256 | Should Be (Get-FileHash -LiteralPath (Join-Path $packageRoot "freeremotedesk-windows.exe") -Algorithm SHA256).Hash
        $manifest.payloadSha256 | Should Match '^[0-9A-F]{64}$'
    }

    It "rejects an old GUI executable mixed with the staged codec payload" {
        $application = Join-Path $packageRoot "freeremotedesk-windows.exe"
        $backup = Join-Path $TestDrive "freeremotedesk-windows.original.exe"
        Copy-Item -LiteralPath $application -Destination $backup
        try {
            Set-Content -LiteralPath $application -Value "old executable bytes" -Encoding UTF8
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "payload.*不匹配|实际 staged bytes 不匹配"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $application -Force
        }
    }

    It "rejects a staging root when trusted installation is required" {
        $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot -RequireTrustedInstall 2>&1
        $LASTEXITCODE | Should Not Be 0
        ($output -join "`n") | Should Match "受信安装路径检查失败"
    }

    It "detects Windows without relying on the optional OS environment variable" {
        $bomVerifier = Join-Path $TestDrive "verify-windows-package.bom.ps1"
        $bom = [Text.Encoding]::UTF8.GetPreamble()
        $verifierBytes = [IO.File]::ReadAllBytes($verifier)
        $hasBom = $verifierBytes.Length -ge 3 -and
            $verifierBytes[0] -eq 0xEF -and $verifierBytes[1] -eq 0xBB -and $verifierBytes[2] -eq 0xBF
        $windowsPowerShellBytes = if ($hasBom) { $verifierBytes } else { [byte[]]@($bom + $verifierBytes) }
        [IO.File]::WriteAllBytes($bomVerifier, $windowsPowerShellBytes)

        $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
        $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
        $pwsh = (Get-Command pwsh.exe -CommandType Application -ErrorAction Stop).Source
        $hosts = @(
            @{ Name = "pwsh 7"; Path = $pwsh; Verifier = $verifier },
            @{ Name = "Windows PowerShell 5.1"; Path = $systemPowerShell; Verifier = $bomVerifier }
        )
        $hadOs = Test-Path Env:OS
        $oldOs = $env:OS
        Remove-Item Env:OS -ErrorAction SilentlyContinue
        try {
            foreach ($hostUnderTest in $hosts) {
                $hostPath = $hostUnderTest.Path
                $hostVerifier = $hostUnderTest.Verifier
                $output = & $hostPath -NoProfile -ExecutionPolicy Bypass -File `
                    $hostVerifier -PackageRoot $packageRoot -RequireTrustedInstall 2>&1
                $exitCode = $LASTEXITCODE
                $message = $output -join "`n"

                $exitCode | Should Not Be 0
                $message | Should Match "安装目录必须为"
                $message | Should Not Match "仅支持 Windows"
            }
        }
        finally {
            if ($hadOs) {
                $env:OS = $oldOs
            }
            else {
                Remove-Item Env:OS -ErrorAction SilentlyContinue
            }
        }
    }

    It "plans installation only for the fixed Program Files product directory" {
        $output = & pwsh -NoProfile -File $installer -PackageRoot $packageRoot -WhatIf 2>&1
        $LASTEXITCODE | Should Be 0
        $programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
        ($output -join "`n") | Should Match ([regex]::Escape((Join-Path $programFiles "FreeRemoteDesk")))
        $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
        $elevationHost = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
        ($output -join "`n") | Should Match ([regex]::Escape($elevationHost))
    }

    It "rejects a tampered package before installation or elevation" {
        $application = Join-Path $packageRoot "freeremotedesk-windows.exe"
        $backup = Join-Path $TestDrive "freeremotedesk-windows.preinstall.exe"
        Copy-Item -LiteralPath $application -Destination $backup
        try {
            Add-Content -LiteralPath $application -Value "tampered before install" -Encoding UTF8
            $output = & pwsh -NoProfile -File $installer -PackageRoot $packageRoot -WhatIf 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "安装前 package 验证失败"
        }
        finally {
            Copy-Item -LiteralPath $backup -Destination $application -Force
        }
    }

    It "does not execute a PATH-shadowed PowerShell host during package preflight" {
        $fakeBin = Join-Path $TestDrive "fake-pwsh-bin"
        $protectedRoot = Join-Path $TestDrive "path-shadow-root"
        $protectedTools = Join-Path $protectedRoot "tools"
        New-Item -ItemType Directory -Force -Path $fakeBin | Out-Null
        New-Item -ItemType Directory -Force -Path $protectedTools | Out-Null
        Copy-Item -LiteralPath (Join-Path $env:SystemRoot "System32\cmd.exe") -Destination (Join-Path $fakeBin "pwsh.exe")
        foreach ($name in @("install-windows-package.ps1", "verify-windows-package.ps1")) {
            Copy-Utf8ScriptForWindowsPowerShell `
                -Source (Join-Path $repoRoot "tools\$name") `
                -Destination (Join-Path $protectedTools $name)
        }
        $oldPath = $env:Path
        try {
            $env:Path = "$fakeBin;$oldPath"
            $hostPath = (Get-Process -Id $PID).Path
            $output = & $hostPath -NoProfile -File `
                (Join-Path $protectedTools "install-windows-package.ps1") `
                -PackageRoot $packageRoot -WhatIf 2>&1
            $LASTEXITCODE | Should Be 0
            ($output -join "`n") | Should Match "Windows package verification passed"
        }
        finally {
            $env:Path = $oldPath
        }
    }

    It "parses the protected UTF-8 scripts with the fixed Windows PowerShell host" {
        $protectedRoot = Join-Path $TestDrive "protected-installer"
        $protectedTools = Join-Path $protectedRoot "tools"
        New-Item -ItemType Directory -Force -Path $protectedTools | Out-Null
        $bom = [Text.Encoding]::UTF8.GetPreamble()
        foreach ($name in @("install-windows-package.ps1", "verify-windows-package.ps1")) {
            $source = Join-Path $repoRoot "tools\$name"
            $destination = Join-Path $protectedTools $name
            [IO.File]::WriteAllBytes($destination, [byte[]]@($bom + [IO.File]::ReadAllBytes($source)))
        }
        $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
        $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"

        $output = & $systemPowerShell -NoProfile -ExecutionPolicy Bypass -File `
            (Join-Path $protectedTools "install-windows-package.ps1") `
            -PackageRoot $packageRoot -WhatIf 2>&1

        ($output -join "`n") | Should Match "Windows package installation planned"
        $LASTEXITCODE | Should Be 0
    }

    It "rejects an approved DLL shadow copy in the package root" {
        $shadow = Join-Path $packageRoot "avcodec-62.dll"
        Copy-Item -LiteralPath (Join-Path $codecRoot "avcodec-62.dll") -Destination $shadow
        try {
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "版本化 codec 目录之外存在未批准 DLL"
        }
        finally {
            Remove-Item -LiteralPath $shadow -Force
        }
    }

    It "rejects an unapproved imported runtime DLL in the package root" {
        $shadow = Join-Path $packageRoot "VCRUNTIME140.dll"
        Set-Content -LiteralPath $shadow -Value "unapproved runtime shadow" -Encoding UTF8
        try {
            $output = & pwsh -NoProfile -File $verifier -PackageRoot $packageRoot 2>&1
            $LASTEXITCODE | Should Not Be 0
            ($output -join "`n") | Should Match "版本化 codec 目录之外存在未批准 DLL"
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

    It "publishes the exact corresponding-source ZIP in non-hidden release staging" {
        (Test-Path -LiteralPath $correspondingSourceAsset -PathType Leaf) | Should Be $true
        $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $packageRoot "ffmpeg-manifest.json") | ConvertFrom-Json
        (Get-FileHash -LiteralPath $correspondingSourceAsset -Algorithm SHA256).Hash | Should Be $manifest.correspondingSource.sha256
    }
}

Describe "Windows elevation bootstrap security boundary" {
    BeforeAll {
        $bootstrapBuilderForHost = Join-Path $TestDrive "new-windows-installer-bootstrap.ps1"
        Copy-Utf8ScriptForWindowsPowerShell `
            -Source $bootstrapBuilder `
            -Destination $bootstrapBuilderForHost
    }

    It "binds elevated installer current directory to the hash-bound package root instead of System32" {
        $cleanPackage = Join-Path $TestDrive "clean package root"
        New-Item -ItemType Directory -Path $cleanPackage | Out-Null
        $marker = Join-Path $TestDrive "package-cwd.marker"
        $installerText = @"
[CmdletBinding()]
param([string]`$PackageRoot, [switch]`$Elevated, [byte[]]`$TrustedVerifierBytes)
if (-not (Test-Path -LiteralPath (Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::System)) 'kernel32.dll') -PathType Leaf)) { throw 'System32 DLL fixture is unavailable' }
if (-not ([IO.Path]::GetFullPath((Get-Location).Path).TrimEnd('\') -eq [IO.Path]::GetFullPath(`$PackageRoot).TrimEnd('\'))) { throw "elevated cwd was not package root: `$((Get-Location).Path)" }
if (@(Get-ChildItem -LiteralPath (Get-Location).Path -File -Filter '*.dll').Count -ne 0) { throw 'clean package root unexpectedly contains a DLL' }
[IO.File]::WriteAllText('$($marker.Replace("'", "''"))', (Get-Location).Path, [Text.Encoding]::UTF8)
"@
        $plan = $null
        try {
            $plan = & $bootstrapBuilderForHost `
                -InstallerBytes ([Text.Encoding]::UTF8.GetBytes($installerText)) `
                -VerifierBytes ([Text.Encoding]::UTF8.GetBytes("trusted verifier")) `
                -PackageRoot $cleanPackage `
                -StagingParent $TestDrive
            $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
            $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
            Push-Location $systemDirectory
            try {
                $output = & $systemPowerShell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $plan.EncodedCommand 2>&1
            }
            finally {
                Pop-Location
            }

            $LASTEXITCODE | Should Be 0
            [IO.Path]::GetFullPath((Get-Content -Raw -LiteralPath $marker)).TrimEnd('\') | Should Be ([IO.Path]::GetFullPath($cleanPackage).TrimEnd('\'))
            ($output -join "`n") | Should Match "FreeRemoteDesk 管理员 payload 已通过校验"
        }
        finally {
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.PayloadPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.PayloadPath -Force
            }
            if ($null -ne $plan -and $null -ne $plan.ResultPath -and
                (Test-Path -LiteralPath $plan.ResultPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.ResultPath -Force
            }
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.StagingRoot -PathType Container)) {
                Remove-Item -LiteralPath $plan.StagingRoot -Force
            }
        }
    }

    It "returns a bounded structured administrator error without inherited secrets" {
        $secretSentinel = "FRD_TEST_SECRET_9b17f34b"
        $oldSecret = $env:FRD_PASSWORD
        $env:FRD_PASSWORD = $secretSentinel
        $installerText = @"
[CmdletBinding()]
param([string]`$PackageRoot, [switch]`$Elevated, [byte[]]`$TrustedVerifierBytes)
Write-Output 'diagnostic before failure'
throw 'bounded administrator failure detail'
"@
        $plan = $null
        try {
            $plan = & $bootstrapBuilderForHost `
                -InstallerBytes ([Text.Encoding]::UTF8.GetBytes($installerText)) `
                -VerifierBytes ([Text.Encoding]::UTF8.GetBytes("trusted verifier")) `
                -PackageRoot $TestDrive `
                -StagingParent $TestDrive
            $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
            $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
            $output = & $systemPowerShell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $plan.EncodedCommand 2>&1

            $LASTEXITCODE | Should Not Be 0
            $resultText = Get-Content -Raw -Encoding UTF8 -LiteralPath $plan.ResultPath
            $result = $resultText | ConvertFrom-Json
            $result.schema | Should Be "freeremotedesk.windows.install-result.v1"
            $result.status | Should Be "error"
            $result.stage | Should Be "installer_execution"
            $result.message | Should Match "bounded administrator failure detail"
            ($result.stdout -join "`n") | Should Match "diagnostic before failure"
            $resultText | Should Not Match ([regex]::Escape($secretSentinel))
            $resultText.Length | Should BeLessThan 8192
            ($output -join "`n") | Should Not Match ([regex]::Escape($secretSentinel))
        }
        finally {
            if ($null -eq $oldSecret) {
                Remove-Item Env:FRD_PASSWORD -ErrorAction SilentlyContinue
            }
            else {
                $env:FRD_PASSWORD = $oldSecret
            }
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.PayloadPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.PayloadPath -Force
            }
            if ($null -ne $plan -and $null -ne $plan.ResultPath -and
                (Test-Path -LiteralPath $plan.ResultPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.ResultPath -Force
            }
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.StagingRoot -PathType Container)) {
                Remove-Item -LiteralPath $plan.StagingRoot -Force
            }
        }
    }

    It "runs the real elevated installer from hash-checked in-memory bytes without a script path" {
        $installerFileBytes = [IO.File]::ReadAllBytes($installer)
        $installerHasBom = $installerFileBytes.Length -ge 3 -and
            $installerFileBytes[0] -eq 0xEF -and
            $installerFileBytes[1] -eq 0xBB -and
            $installerFileBytes[2] -eq 0xBF
        $installerBytes = if ($installerHasBom) {
            $installerFileBytes[3..($installerFileBytes.Length - 1)]
        }
        else {
            $installerFileBytes
        }
        $verifierFileBytes = [IO.File]::ReadAllBytes($verifier)
        $verifierHasBom = $verifierFileBytes.Length -ge 3 -and
            $verifierFileBytes[0] -eq 0xEF -and
            $verifierFileBytes[1] -eq 0xBB -and
            $verifierFileBytes[2] -eq 0xBF
        $verifierBytes = if ($verifierHasBom) {
            $verifierFileBytes[3..($verifierFileBytes.Length - 1)]
        }
        else {
            $verifierFileBytes
        }
        $installerScript = [ScriptBlock]::Create([Text.Encoding]::UTF8.GetString($installerBytes))

        $output = & $installerScript `
            -PackageRoot $packageRoot `
            -Elevated `
            -TrustedVerifierBytes $verifierBytes `
            -WhatIf *>&1

        ($output -join "`n") | Should Match "Windows package installation planned"
    }

    It "keeps a large installer payload below the Windows command-line limit and executes the hash-bound bytes" {
        $marker = Join-Path $TestDrive "large-bootstrap.marker"
        $packageArgument = Join-Path $TestDrive "package argument with spaces"
        New-Item -ItemType Directory -Path $packageArgument | Out-Null
        $installerText = @"
[CmdletBinding()]
param(
    [Parameter(Mandatory = `$true)][string]`$PackageRoot,
    [switch]`$Elevated,
    [byte[]]`$TrustedVerifierBytes
)
if (-not `$Elevated) { throw 'expected elevated bundle mode' }
if (`$TrustedVerifierBytes.Length -lt 131072) { throw 'verifier payload was truncated' }
[IO.File]::WriteAllText('$($marker.Replace("'", "''"))', `$PackageRoot, [Text.Encoding]::UTF8)
"@
        $largeVerifier = New-Object byte[] 131072
        ([Random]::new(8675309)).NextBytes($largeVerifier)
        $plan = $null
        try {
            $plan = & $bootstrapBuilderForHost `
                -InstallerBytes ([Text.Encoding]::UTF8.GetBytes($installerText)) `
                -VerifierBytes $largeVerifier `
                -PackageRoot $packageArgument `
                -StagingParent $TestDrive

            $plan.EncodedCommand.Length | Should BeLessThan 30000
            $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
            $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
            $output = & $systemPowerShell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $plan.EncodedCommand 2>&1

            $LASTEXITCODE | Should Be 0
            (Get-Content -Raw -LiteralPath $marker) | Should Be $packageArgument
            ($output -join "`n") | Should Match "FreeRemoteDesk 管理员 payload 已通过校验"
        }
        finally {
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.PayloadPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.PayloadPath -Force
            }
            if ($null -ne $plan -and $null -ne $plan.ResultPath -and
                (Test-Path -LiteralPath $plan.ResultPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.ResultPath -Force
            }
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.StagingRoot -PathType Container)) {
                Remove-Item -LiteralPath $plan.StagingRoot -Force
            }
        }
    }

    It "rejects a staged elevation bundle changed after its hash was anchored" {
        $marker = Join-Path $TestDrive "tampered-bootstrap.marker"
        $installerText = @"
[CmdletBinding()]
param([string]`$PackageRoot, [switch]`$Elevated, [byte[]]`$TrustedVerifierBytes)
[IO.File]::WriteAllText('$($marker.Replace("'", "''"))', 'executed')
"@
        $plan = $null
        try {
            $plan = & $bootstrapBuilderForHost `
                -InstallerBytes ([Text.Encoding]::UTF8.GetBytes($installerText)) `
                -VerifierBytes ([Text.Encoding]::UTF8.GetBytes("trusted verifier")) `
                -PackageRoot $TestDrive `
                -StagingParent $TestDrive
            [IO.File]::AppendAllText($plan.PayloadPath, "tampered")

            $systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
            $systemPowerShell = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
            $output = & $systemPowerShell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $plan.EncodedCommand 2>&1

            $LASTEXITCODE | Should Not Be 0
            (Test-Path -LiteralPath $marker) | Should Be $false
            $result = Get-Content -Raw -Encoding UTF8 -LiteralPath $plan.ResultPath | ConvertFrom-Json
            $result.status | Should Be "error"
            $result.stage | Should Be "payload_validation"
            $result.message | Should Match "管理员 elevation payload SHA-256 不匹配"
            ($output -join "`n") | Should Not Match "管理员 elevation payload SHA-256 不匹配"
        }
        finally {
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.PayloadPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.PayloadPath -Force
            }
            if ($null -ne $plan -and $null -ne $plan.ResultPath -and
                (Test-Path -LiteralPath $plan.ResultPath -PathType Leaf)) {
                Remove-Item -LiteralPath $plan.ResultPath -Force
            }
            if ($null -ne $plan -and (Test-Path -LiteralPath $plan.StagingRoot -PathType Container)) {
                Remove-Item -LiteralPath $plan.StagingRoot -Force
            }
        }
    }
}
