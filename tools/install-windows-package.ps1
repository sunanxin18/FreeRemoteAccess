[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = "Medium")]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot,
    [switch]$Elevated,
    [Parameter(DontShow = $true)]
    [byte[]]$TrustedVerifierBytes
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = $null
$verifier = $null
$bootstrapBuilder = $null
if (-not $Elevated) {
    if ([string]::IsNullOrWhiteSpace($PSScriptRoot)) {
        throw "非管理员安装必须从固定 installer 文件启动"
    }
    $repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
    $verifier = Join-Path $repoRoot "tools\verify-windows-package.ps1"
    $bootstrapBuilder = Join-Path $repoRoot "tools\new-windows-installer-bootstrap.ps1"
}
$expectedVerifierSha256 = "74D50DFA2786AD3712214E927A429CA4AB947DE9EAFD86C67089B265ACA7D968"
$expectedBootstrapBuilderSha256 = "4CA4924D92A42704DA14BB1F8709B12681CF13264FA4530F4DFB845588AE16D0"
$package = [IO.Path]::GetFullPath($PackageRoot)
$systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
$elevationHost = Join-Path $systemDirectory "WindowsPowerShell\v1.0\powershell.exe"
$icacls = Join-Path $systemDirectory "icacls.exe"
$programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
$installRoot = [IO.Path]::GetFullPath((Join-Path $programFiles "FreeRemoteDesk"))
$programFilesRoot = [IO.Path]::GetFullPath($programFiles).TrimEnd('\') + '\'

if (-not $installRoot.StartsWith($programFilesRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "固定安装目录不在 Program Files 下: $installRoot"
}
if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
    throw "windows-x86_64 package 必须由 64 位 PowerShell 安装"
}
foreach ($systemTool in @($elevationHost, $icacls)) {
    if (-not (Test-Path -LiteralPath $systemTool -PathType Leaf)) {
        throw "系统安装工具不存在: $systemTool"
    }
}
$hostSignature = Get-AuthenticodeSignature -LiteralPath $elevationHost
if ($hostSignature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
    $null -eq $hostSignature.SignerCertificate -or
    -not $hostSignature.SignerCertificate.Subject.Contains("O=Microsoft Corporation")) {
    throw "系统 PowerShell 主机签名无效"
}

$verifierFileBytes = if ($Elevated) {
    if ($null -eq $TrustedVerifierBytes -or $TrustedVerifierBytes.Length -eq 0) {
        throw "管理员安装缺少由父进程 hash 锚定的 package verifier"
    }
    $TrustedVerifierBytes
}
else {
    [IO.File]::ReadAllBytes($verifier)
}
$hasUtf8Bom = $verifierFileBytes.Length -ge 3 -and
    $verifierFileBytes[0] -eq 0xEF -and $verifierFileBytes[1] -eq 0xBB -and $verifierFileBytes[2] -eq 0xBF
$verifierBytes = if ($hasUtf8Bom) { $verifierFileBytes[3..($verifierFileBytes.Length - 1)] } else { $verifierFileBytes }
$sha = [Security.Cryptography.SHA256]::Create()
try {
    $actualVerifierSha256 = (($sha.ComputeHash($verifierBytes) | ForEach-Object { $_.ToString("X2") }) -join "")
}
finally {
    $sha.Dispose()
}
if ($actualVerifierSha256 -cne $expectedVerifierSha256) {
    throw "package verifier 与 installer 固定版本不匹配"
}
$verifierScript = [ScriptBlock]::Create([Text.Encoding]::UTF8.GetString($verifierBytes))

try {
    $savedWhatIfPreference = $WhatIfPreference
    $WhatIfPreference = $false
    & $verifierScript -PackageRoot $package
}
catch {
    throw "安装前 package 验证失败: $($_.Exception.Message)"
}
finally {
    $WhatIfPreference = $savedWhatIfPreference
}

if (-not $PSCmdlet.ShouldProcess($installRoot, "安装或升级完整 FreeRemoteDesk Windows payload")) {
    if ($WhatIfPreference) {
        Write-Host "Windows package installation planned: $installRoot"
        Write-Host "Trusted elevation host: $elevationHost"
    }
    return
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdministrator) {
    if ($Elevated) {
        throw "UAC 提权后仍未获得管理员权限"
    }
    $installerBytes = [Text.Encoding]::UTF8.GetBytes($MyInvocation.MyCommand.ScriptBlock.ToString())
    $bootstrapBuilderFileBytes = [IO.File]::ReadAllBytes($bootstrapBuilder)
    $builderHasUtf8Bom = $bootstrapBuilderFileBytes.Length -ge 3 -and
        $bootstrapBuilderFileBytes[0] -eq 0xEF -and
        $bootstrapBuilderFileBytes[1] -eq 0xBB -and
        $bootstrapBuilderFileBytes[2] -eq 0xBF
    $bootstrapBuilderBytesWithCheckoutLineEndings = if ($builderHasUtf8Bom) {
        $bootstrapBuilderFileBytes[3..($bootstrapBuilderFileBytes.Length - 1)]
    }
    else {
        $bootstrapBuilderFileBytes
    }
    $bootstrapBuilderText = [Text.Encoding]::UTF8.GetString($bootstrapBuilderBytesWithCheckoutLineEndings).Replace("`r`n", "`n")
    $bootstrapBuilderBytes = [Text.Encoding]::UTF8.GetBytes($bootstrapBuilderText)
    $builderSha = [Security.Cryptography.SHA256]::Create()
    try {
        $actualBootstrapBuilderSha256 = (($builderSha.ComputeHash($bootstrapBuilderBytes) |
                ForEach-Object { $_.ToString("X2") }) -join "")
    }
    finally {
        $builderSha.Dispose()
    }
    if ($actualBootstrapBuilderSha256 -cne $expectedBootstrapBuilderSha256) {
        throw "elevation bootstrap builder 与 installer 固定版本不匹配"
    }

    $bootstrapBuilderScript = [ScriptBlock]::Create([Text.Encoding]::UTF8.GetString($bootstrapBuilderBytes))
    $plan = $null
    try {
        $plan = & $bootstrapBuilderScript `
            -InstallerBytes $installerBytes `
            -VerifierBytes $verifierBytes `
            -PackageRoot $package
        if ($plan.EncodedCommand.Length -ge 30000) {
            throw "管理员安装 bootstrap 超过 Windows 安全命令行上限"
        }
        $child = Start-Process -FilePath $elevationHost -Verb RunAs -WindowStyle Hidden -ArgumentList @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", $plan.EncodedCommand
        ) -Wait -PassThru
        if ($child.ExitCode -ne 0) {
            throw "管理员安装进程失败，退出码 $($child.ExitCode)"
        }
    }
    finally {
        if ($null -ne $plan -and (Test-Path -LiteralPath $plan.PayloadPath -PathType Leaf)) {
            [IO.File]::Delete($plan.PayloadPath)
        }
        if ($null -ne $plan -and (Test-Path -LiteralPath $plan.StagingRoot -PathType Container)) {
            [IO.Directory]::Delete($plan.StagingRoot, $false)
        }
    }
    return
}

$suffix = "$PID-$([Guid]::NewGuid().ToString('N'))"
$attempt = Join-Path $programFiles ".FreeRemoteDesk.stage-$suffix"
$backup = Join-Path $programFiles ".FreeRemoteDesk.previous-$suffix"
$published = $false
$backedUp = $false
$installMutex = [Threading.Mutex]::new($false, "Global\FreeRemoteDesk.Windows.PackageInstall.v1")
$lockHeld = $false

foreach ($candidate in @($attempt, $backup)) {
    $full = [IO.Path]::GetFullPath($candidate)
    if (-not $full.StartsWith($programFilesRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝在 Program Files 之外操作临时安装路径: $full"
    }
}

try {
    try {
        $lockHeld = $installMutex.WaitOne([TimeSpan]::FromMinutes(2))
    }
    catch [Threading.AbandonedMutexException] {
        $lockHeld = $true
    }
    if (-not $lockHeld) {
        throw "另一个 FreeRemoteDesk 安装正在运行"
    }
    New-Item -ItemType Directory -Path $attempt | Out-Null
    & $icacls $attempt /inheritance:r /grant:r `
        "*S-1-5-18:(OI)(CI)(F)" `
        "*S-1-5-32-544:(OI)(CI)(F)" `
        "*S-1-5-32-545:(OI)(CI)(RX)" /Q
    if ($LASTEXITCODE -ne 0) {
        throw "无法为临时安装目录设置受信 DACL"
    }
    foreach ($entry in @(Get-ChildItem -LiteralPath $package -Force)) {
        Copy-Item -LiteralPath $entry.FullName -Destination $attempt -Recurse -Force
    }
    & $icacls $attempt /setowner "*S-1-5-32-544" /T /C /Q
    if ($LASTEXITCODE -ne 0) {
        throw "无法将临时安装 payload owner 设置为 Administrators"
    }
    try {
        & $verifierScript -PackageRoot $attempt
    }
    catch {
        throw "Program Files 临时安装 payload 验证失败: $($_.Exception.Message)"
    }

    if (Test-Path -LiteralPath $installRoot) {
        Move-Item -LiteralPath $installRoot -Destination $backup
        $backedUp = $true
    }
    Move-Item -LiteralPath $attempt -Destination $installRoot
    $published = $true

    try {
        & $verifierScript -PackageRoot $installRoot -RequireTrustedInstall
    }
    catch {
        throw "安装后 package 或受信路径验证失败: $($_.Exception.Message)"
    }
}
catch {
    $failure = $_
    $rollbackFailures = @()
    if ($published -and (Test-Path -LiteralPath $installRoot)) {
        try { Remove-Item -LiteralPath $installRoot -Recurse -Force }
        catch { $rollbackFailures += $_.Exception.Message }
    }
    if ($backedUp -and (Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $installRoot)) {
        try { Move-Item -LiteralPath $backup -Destination $installRoot }
        catch { $rollbackFailures += $_.Exception.Message }
    }
    if ($rollbackFailures.Count -ne 0) {
        throw "安装失败: $($failure.Exception.Message)；回滚也失败: $($rollbackFailures -join '; ')"
    }
    throw $failure
}
finally {
    if (Test-Path -LiteralPath $attempt) {
        Remove-Item -LiteralPath $attempt -Recurse -Force
    }
    if ($lockHeld) {
        $installMutex.ReleaseMutex()
    }
    $installMutex.Dispose()
}

if (Test-Path -LiteralPath $backup) {
    Remove-Item -LiteralPath $backup -Recurse -Force
}

Write-Host "FreeRemoteDesk installed and trusted-path verified: $installRoot"
