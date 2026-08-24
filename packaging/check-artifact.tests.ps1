$ErrorActionPreference = 'Stop'
$checker = Join-Path $PSScriptRoot 'check-artifact.ps1'
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("freeremoteaccess-package-test-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $testRoot | Out-Null

try {
    $empty = Join-Path $testRoot 'FreeRemoteAccess-0.1.0-windows-x64.msi'
    [IO.File]::WriteAllBytes($empty, [byte[]]::new(0))
    try {
        & $checker -Path $empty -Version '0.1.0' -Platform windows -Arch x64
        throw '零字节产物未被拒绝'
    } catch {
        if ($_.Exception.Message -eq '零字节产物未被拒绝') { throw }
    }

    $wrongName = Join-Path $testRoot 'unexpected.msi'
    [IO.File]::WriteAllBytes($wrongName, [byte[]](1, 2, 3))
    try {
        & $checker -Path $wrongName -Version '0.1.0' -Platform windows -Arch x64
        throw '错误名称未被拒绝'
    } catch {
        if ($_.Exception.Message -eq '错误名称未被拒绝') { throw }
    }

    $valid = Join-Path $testRoot 'FreeRemoteAccess-0.1.0-windows-x64.exe'
    [IO.File]::WriteAllBytes($valid, [byte[]](1, 2, 3, 4))
    $sidecar = & $checker -Path $valid -Version '0.1.0' -Platform windows -Arch x64
    $expectedHash = (Get-FileHash -LiteralPath $valid -Algorithm SHA256).Hash.ToLowerInvariant()
    $actual = [IO.File]::ReadAllText($sidecar)
    if ($actual -ne "$expectedHash  $([IO.Path]::GetFileName($valid))`n") {
        throw 'SHA-256 sidecar 内容不符合约定'
    }
    Write-Output 'artifact-contract-tests: ok'
} finally {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
}
