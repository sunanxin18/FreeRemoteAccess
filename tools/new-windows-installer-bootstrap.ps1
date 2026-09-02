[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [byte[]]$InstallerBytes,
    [Parameter(Mandatory = $true)]
    [byte[]]$VerifierBytes,
    [Parameter(Mandatory = $true)]
    [string]$PackageRoot,
    [string]$StagingParent = [IO.Path]::GetTempPath()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($InstallerBytes.Length -eq 0 -or $VerifierBytes.Length -eq 0) {
    throw "管理员安装 payload 脚本不得为空"
}

$parent = [IO.Path]::GetFullPath($StagingParent).TrimEnd('\')
if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    throw "管理员安装暂存父目录不存在: $parent"
}
$parentItem = Get-Item -Force -LiteralPath $parent
if (($parentItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "管理员安装暂存父目录不得为 reparse point: $parent"
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$userSid = $identity.User
if ($null -eq $userSid) {
    throw "当前 Windows 身份缺少 SID"
}
$systemSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-18")
$administratorsSid = [Security.Principal.SecurityIdentifier]::new("S-1-5-32-544")
$inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
    [Security.AccessControl.InheritanceFlags]::ObjectInherit
$propagation = [Security.AccessControl.PropagationFlags]::None
$allow = [Security.AccessControl.AccessControlType]::Allow
$fullControl = [Security.AccessControl.FileSystemRights]::FullControl

$stagingRoot = Join-Path $parent (".FreeRemoteDesk.install-bootstrap-" + [Guid]::NewGuid().ToString("N"))
$payloadPath = Join-Path $stagingRoot "elevation-payload.ps1"
$created = $false
try {
    [IO.Directory]::CreateDirectory($stagingRoot) | Out-Null
    $created = $true
    $directorySecurity = New-Object Security.AccessControl.DirectorySecurity
    $directorySecurity.SetOwner($userSid)
    $directorySecurity.SetAccessRuleProtection($true, $false)
    foreach ($sid in @($userSid, $systemSid, $administratorsSid)) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule(
            $sid,
            $fullControl,
            $inheritance,
            $propagation,
            $allow
        )
        [void]$directorySecurity.AddAccessRule($rule)
    }
    Set-Acl -LiteralPath $stagingRoot -AclObject $directorySecurity

    $installerBlob = [Convert]::ToBase64String($InstallerBytes)
    $verifierBlob = [Convert]::ToBase64String($VerifierBytes)
    $packageBlob = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([IO.Path]::GetFullPath($PackageRoot)))
    $bundle = @"
`$ErrorActionPreference = 'Stop'
`$installerBytes = [Convert]::FromBase64String('$installerBlob')
`$verifierBytes = [Convert]::FromBase64String('$verifierBlob')
`$package = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$packageBlob'))
`$installer = [ScriptBlock]::Create([Text.Encoding]::UTF8.GetString(`$installerBytes))
& `$installer -PackageRoot `$package -Elevated -TrustedVerifierBytes `$verifierBytes
"@
    $bundleBytes = [Text.Encoding]::UTF8.GetBytes($bundle)
    $stream = [IO.FileStream]::new(
        $payloadPath,
        [IO.FileMode]::CreateNew,
        [IO.FileAccess]::Write,
        [IO.FileShare]::None
    )
    try {
        $stream.Write($bundleBytes, 0, $bundleBytes.Length)
        $stream.Flush($true)
    }
    finally {
        $stream.Dispose()
    }

    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $payloadHashBytes = $sha.ComputeHash($bundleBytes)
    }
    finally {
        $sha.Dispose()
    }
    $payloadHash = [Convert]::ToBase64String($payloadHashBytes)
    $payloadPathBlob = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payloadPath))
    $userSidBlob = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($userSid.Value))

    $launcher = @"
`$ErrorActionPreference='Stop'
try {
`$p=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$payloadPathBlob'))
`$expected=[Convert]::FromBase64String('$payloadHash')
`$userSid=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$userSidBlob'))
`$root=[IO.Path]::GetDirectoryName(`$p)
if ([IO.Path]::GetFileName(`$p) -cne 'elevation-payload.ps1' -or [IO.Path]::GetFileName(`$root) -cnotmatch '^\.FreeRemoteDesk\.install-bootstrap-[0-9a-f]{32}$') { throw '管理员 elevation payload 路径契约不匹配' }
function Get-Sid([string]`$value) { try { return ([Security.Principal.NTAccount]::new(`$value)).Translate([Security.Principal.SecurityIdentifier]).Value } catch { return ([Security.Principal.SecurityIdentifier]::new(`$value)).Value } }
function Assert-Object([string]`$path,[bool]`$directory) {
`$item=Get-Item -Force -LiteralPath `$path -ErrorAction Stop
if ((`$item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw '管理员 elevation payload 拒绝 reparse point' }
if (`$item.PSIsContainer -ne `$directory) { throw '管理员 elevation payload 对象类型不匹配' }
`$acl=Get-Acl -LiteralPath `$path -ErrorAction Stop
`$allowed=@(`$userSid,'S-1-5-18','S-1-5-32-544')
if (`$allowed -cnotcontains (Get-Sid ([string]`$acl.Owner))) { throw '管理员 elevation payload owner 不受信' }
`$sddl=`$acl.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::Access)
if ([string]::IsNullOrWhiteSpace(`$sddl) -or `$sddl.Contains('NO_ACCESS_CONTROL')) { throw '管理员 elevation payload 缺少 DACL' }
[uint64]`$danger=0x10000000L -bor 0x40000000L -bor 0x00010000L -bor 0x00000002L -bor 0x00000004L -bor 0x00000010L -bor 0x00000100L -bor 0x00040000L -bor 0x00080000L
foreach (`$rule in @(`$acl.GetAccessRules(`$true,`$true,[Security.Principal.SecurityIdentifier]))) {
if ((`$rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) { continue }
[uint64]`$mask=([uint64]([int64]`$rule.FileSystemRights)) -band 0xFFFFFFFFL
if ((`$mask -band `$danger) -ne 0 -and (`$rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or `$allowed -cnotcontains `$rule.IdentityReference.Value)) { throw '管理员 elevation payload DACL 授予了不安全的修改权限' }
}
}
Assert-Object `$root `$true
Assert-Object `$p `$false
`$stream=[IO.FileStream]::new(`$p,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::None)
try { `$memory=New-Object IO.MemoryStream; try { `$stream.CopyTo(`$memory); `$bytes=`$memory.ToArray() } finally { `$memory.Dispose() }; Assert-Object `$root `$true; Assert-Object `$p `$false } finally { `$stream.Dispose() }
`$sha=[Security.Cryptography.SHA256]::Create(); try { `$actual=`$sha.ComputeHash(`$bytes) } finally { `$sha.Dispose() }
if (`$actual.Length -ne `$expected.Length) { throw '管理员 elevation payload SHA-256 不匹配' }
`$difference=0
for (`$i=0; `$i -lt `$actual.Length; `$i++) { `$difference=`$difference -bor (`$actual[`$i] -bxor `$expected[`$i]) }
if (`$difference -ne 0) { throw '管理员 elevation payload SHA-256 不匹配' }
& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString(`$bytes)))
Write-Output 'FreeRemoteDesk 管理员 payload 已通过校验'
exit 0
} catch { Write-Error `$_; exit 1 }
"@
    $encodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($launcher))
    if ($encodedCommand.Length -ge 30000) {
        throw "管理员 bootstrap 超过 Windows 安全命令行上限"
    }

    [PSCustomObject]@{
        StagingRoot = $stagingRoot
        PayloadPath = $payloadPath
        PayloadSha256 = (($payloadHashBytes | ForEach-Object { $_.ToString("X2") }) -join "")
        EncodedCommand = $encodedCommand
        UserSid = $userSid.Value
    }
}
catch {
    if ($created -and (Test-Path -LiteralPath $payloadPath -PathType Leaf)) {
        [IO.File]::Delete($payloadPath)
    }
    if ($created -and (Test-Path -LiteralPath $stagingRoot -PathType Container)) {
        [IO.Directory]::Delete($stagingRoot, $false)
    }
    throw
}
