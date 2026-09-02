[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = "Medium")]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot,
    [switch]$Elevated
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$verifier = Join-Path $repoRoot "tools\verify-windows-package.ps1"
$expectedVerifierSha256 = "74D50DFA2786AD3712214E927A429CA4AB947DE9EAFD86C67089B265ACA7D968"
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

$verifierFileBytes = [IO.File]::ReadAllBytes($verifier)
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
    function ConvertTo-GzipBase64([byte[]]$InputBytes) {
        $output = [IO.MemoryStream]::new()
        $gzip = [IO.Compression.GZipStream]::new($output, [IO.Compression.CompressionMode]::Compress, $true)
        try {
            $gzip.Write($InputBytes, 0, $InputBytes.Length)
        }
        finally {
            $gzip.Dispose()
        }
        try {
            return [Convert]::ToBase64String($output.ToArray())
        }
        finally {
            $output.Dispose()
        }
    }

    $forward = @{ package = $package } | ConvertTo-Json -Compress
    $data = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($forward))
    $utf8Bom = [Text.Encoding]::UTF8.GetPreamble()
    $installerBytes = [Text.Encoding]::UTF8.GetBytes($MyInvocation.MyCommand.ScriptBlock.ToString())
    $installerFileBytes = [byte[]]@($utf8Bom + $installerBytes)
    $verifierEmbeddedBytes = [byte[]]@($utf8Bom + $verifierBytes)
    $installerBlob = ConvertTo-GzipBase64 $installerFileBytes
    $verifierBlob = ConvertTo-GzipBase64 $verifierEmbeddedBytes
    $installerSha = [Security.Cryptography.SHA256]::Create()
    try {
        $installerHash = (($installerSha.ComputeHash($installerFileBytes) | ForEach-Object { $_.ToString("X2") }) -join "")
    }
    finally {
        $installerSha.Dispose()
    }
    $verifierFileSha = [Security.Cryptography.SHA256]::Create()
    try {
        $verifierHash = (($verifierFileSha.ComputeHash($verifierEmbeddedBytes) | ForEach-Object { $_.ToString("X2") }) -join "")
    }
    finally {
        $verifierFileSha.Dispose()
    }
    $bootstrap = @"
`$ErrorActionPreference = 'Stop'
function Expand-EmbeddedScript([string]`$Value) {
    `$compressed = [Convert]::FromBase64String(`$Value)
    `$input = New-Object IO.MemoryStream(,`$compressed)
    `$gzip = New-Object IO.Compression.GZipStream(`$input, [IO.Compression.CompressionMode]::Decompress)
    `$output = New-Object IO.MemoryStream
    try { `$gzip.CopyTo(`$output); return `$output.ToArray() }
    finally { `$gzip.Dispose(); `$input.Dispose(); `$output.Dispose() }
}
`$programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
`$systemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
`$icacls = Join-Path `$systemDirectory 'icacls.exe'
`$helperRoot = Join-Path `$programFiles ('.FreeRemoteDesk.installer-' + [Guid]::NewGuid().ToString('N'))
try {
    `$argsJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$data')) | ConvertFrom-Json
    `$tools = Join-Path `$helperRoot 'tools'
    New-Item -ItemType Directory -Path `$tools -Force | Out-Null
    & `$icacls `$helperRoot /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)(F)' '*S-1-5-32-544:(OI)(CI)(F)' '*S-1-5-32-545:(OI)(CI)(RX)' /Q
    if (`$LASTEXITCODE -ne 0) { throw '无法保护管理员安装脚本目录' }
    `$installerPath = Join-Path `$tools 'install-windows-package.ps1'
    `$verifierPath = Join-Path `$tools 'verify-windows-package.ps1'
    [IO.File]::WriteAllBytes(`$installerPath, (Expand-EmbeddedScript '$installerBlob'))
    [IO.File]::WriteAllBytes(`$verifierPath, (Expand-EmbeddedScript '$verifierBlob'))
    & `$icacls `$helperRoot /setowner '*S-1-5-32-544' /T /C /Q
    if (`$LASTEXITCODE -ne 0) { throw '无法保护管理员安装脚本 owner' }
    if ((Get-FileHash -LiteralPath `$installerPath -Algorithm SHA256).Hash -cne '$installerHash' -or
        (Get-FileHash -LiteralPath `$verifierPath -Algorithm SHA256).Hash -cne '$verifierHash') {
        throw '管理员安装脚本 payload hash 不匹配'
    }
    & `$installerPath -PackageRoot `$argsJson.package -Elevated
    exit 0
}
catch {
    Write-Error `$_
    exit 1
}
finally {
    if (Test-Path -LiteralPath `$helperRoot) {
        Remove-Item -LiteralPath `$helperRoot -Recurse -Force
    }
}
"@
    $bootstrapBytes = [Text.Encoding]::UTF8.GetBytes($bootstrap)
    $bootstrapOutput = [IO.MemoryStream]::new()
    $bootstrapGzip = [IO.Compression.GZipStream]::new($bootstrapOutput, [IO.Compression.CompressionMode]::Compress, $true)
    try {
        $bootstrapGzip.Write($bootstrapBytes, 0, $bootstrapBytes.Length)
    }
    finally {
        $bootstrapGzip.Dispose()
    }
    try {
        $bootstrapBlob = [Convert]::ToBase64String($bootstrapOutput.ToArray())
    }
    finally {
        $bootstrapOutput.Dispose()
    }
    $launcher = @"
`$compressed = [Convert]::FromBase64String('$bootstrapBlob')
`$input = New-Object IO.MemoryStream(,`$compressed)
`$gzip = New-Object IO.Compression.GZipStream(`$input, [IO.Compression.CompressionMode]::Decompress)
`$output = New-Object IO.MemoryStream
try { `$gzip.CopyTo(`$output); `$code = [Text.Encoding]::UTF8.GetString(`$output.ToArray()) }
finally { `$gzip.Dispose(); `$input.Dispose(); `$output.Dispose() }
& ([ScriptBlock]::Create(`$code))
"@
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($launcher))
    if ($encoded.Length -gt 30000) {
        throw "管理员安装 bootstrap 超过 Windows 安全命令行上限"
    }
    $child = Start-Process -FilePath $elevationHost -Verb RunAs -WindowStyle Hidden -ArgumentList @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", $encoded
    ) -Wait -PassThru
    if ($child.ExitCode -ne 0) {
        throw "管理员安装进程失败，退出码 $($child.ExitCode)"
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
