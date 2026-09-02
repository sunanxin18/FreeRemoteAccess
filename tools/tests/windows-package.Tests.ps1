$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$packageRoot = Join-Path $repoRoot "target\package-test"
$codecRoot = Join-Path $packageRoot "codecs\ffmpeg-8.1.2\windows-x86_64"
$verifier = Join-Path $repoRoot "tools\verify-windows-package.ps1"
$installer = Join-Path $repoRoot "tools\install-windows-package.ps1"
$bootstrapBuilder = Join-Path $repoRoot "tools\new-windows-installer-bootstrap.ps1"
$correspondingSourceAsset = Join-Path $repoRoot "target\release-assets\FreeRemoteDesk-ffmpeg-8.1.2-corresponding-source.zip"

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

    It "binds the Windows GUI executable bytes into the complete payload manifest" {
        $manifest = Get-Content -Raw -LiteralPath (Join-Path $packageRoot "ffmpeg-manifest.json") | ConvertFrom-Json
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
        New-Item -ItemType Directory -Force -Path $fakeBin | Out-Null
        Copy-Item -LiteralPath (Join-Path $env:SystemRoot "System32\cmd.exe") -Destination (Join-Path $fakeBin "pwsh.exe")
        $oldPath = $env:Path
        try {
            $env:Path = "$fakeBin;$oldPath"
            $hostPath = (Get-Process -Id $PID).Path
            $output = & $hostPath -NoProfile -File $installer -PackageRoot $packageRoot -WhatIf 2>&1
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
        $manifest = Get-Content -Raw -LiteralPath (Join-Path $packageRoot "ffmpeg-manifest.json") | ConvertFrom-Json
        (Get-FileHash -LiteralPath $correspondingSourceAsset -Algorithm SHA256).Hash | Should Be $manifest.correspondingSource.sha256
    }
}

Describe "Windows elevation bootstrap security boundary" {
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
            $plan = & $bootstrapBuilder `
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
            $plan = & $bootstrapBuilder `
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
            $plan = & $bootstrapBuilder `
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
            $plan = & $bootstrapBuilder `
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
