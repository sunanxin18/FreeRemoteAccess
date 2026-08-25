[CmdletBinding()]
param(
    [string]$ToolListFixture,
    [string]$CliVersionFixture
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-FixtureText {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$ErrorCode
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw $ErrorCode
    }
    return [IO.File]::ReadAllText((Resolve-Path -LiteralPath $Path).Path)
}

$UsingFixtures = $PSBoundParameters.ContainsKey('ToolListFixture') -or
    $PSBoundParameters.ContainsKey('CliVersionFixture')
if ($UsingFixtures -and -not (
    $PSBoundParameters.ContainsKey('ToolListFixture') -and
    $PSBoundParameters.ContainsKey('CliVersionFixture')
)) {
    throw 'wix_version_mismatch'
}

if ($UsingFixtures) {
    $ToolListText = Read-FixtureText $ToolListFixture 'wix_version_mismatch'
    $CliVersionText = Read-FixtureText $CliVersionFixture 'wix_version_mismatch'
} else {
    $ToolListLines = @(& dotnet tool list --global 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw 'wix_version_mismatch'
    }
    $ToolListText = [string]::Join("`n", [string[]]$ToolListLines)

    $CliVersionLines = @(& wix --version 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw 'wix_version_mismatch'
    }
    $CliVersionText = [string]::Join("`n", [string[]]$CliVersionLines)
}

$WixRows = @(
    $ToolListText -split "`r?`n" |
        Where-Object { $_ -match '^\s*wix(?:\s|$)' }
)
if ($WixRows.Count -ne 1) {
    throw 'wix_version_mismatch'
}
$ToolFields = @($WixRows[0].Trim() -split '\s+')
if ($ToolFields.Count -ne 3 -or
    $ToolFields[0] -cne 'wix' -or
    $ToolFields[1] -cne '4.0.6' -or
    $ToolFields[2] -cne 'wix') {
    throw 'wix_version_mismatch'
}

$CliVersionLines = @(
    $CliVersionText -split "`r?`n" |
        Where-Object { $_ -ne '' }
)
if ($CliVersionLines.Count -ne 1) {
    throw 'wix_version_mismatch'
}
$CliVersion = $CliVersionLines[0]
if ($CliVersion -cnotmatch '^4\.0\.6(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$') {
    throw 'wix_version_mismatch'
}

Write-Output "wix-version: package 4.0.6; cli $CliVersion"
