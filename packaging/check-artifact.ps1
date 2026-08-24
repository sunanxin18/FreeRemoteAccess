param(
    [Parameter(Mandatory = $true)]
    [string]$Path,
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [Parameter(Mandatory = $true)]
    [ValidateSet('windows', 'macos', 'linux')]
    [string]$Platform,
    [Parameter(Mandatory = $true)]
    [string]$Arch
)

$ErrorActionPreference = 'Stop'
$artifact = Get-Item -LiteralPath $Path
$expectedPrefix = "FreeRemoteAccess-$Version-$Platform-$Arch"
if (!$artifact.Name.StartsWith($expectedPrefix, [StringComparison]::Ordinal)) {
    throw "产物名称必须以 $expectedPrefix 开头，实际为 $($artifact.Name)"
}
if ($artifact.Length -le 0) {
    throw "产物不能为空: $($artifact.FullName)"
}

$digest = (Get-FileHash -LiteralPath $artifact.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$sidecar = "$($artifact.FullName).sha256"
[IO.File]::WriteAllText($sidecar, "$digest  $($artifact.Name)`n", [Text.UTF8Encoding]::new($false))
Write-Output $sidecar
