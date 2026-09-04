[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$assetDirectory = Join-Path $repositoryRoot 'assets\ui-icons'
$outputPath = Join-Path $assetDirectory 'material-symbols-rounded-24-400.ttf'
$temporaryRoot = Join-Path $repositoryRoot '.codex-target\material-symbols-rounded'
$fontToolsTarget = Join-Path $temporaryRoot 'fonttools-4.63.0'
$upstreamPath = Join-Path $temporaryRoot 'material-symbols-rounded-upstream.ttf'
$codepointsPath = Join-Path $temporaryRoot 'material-symbols-rounded-upstream.codepoints'
$pythonScriptPath = Join-Path $temporaryRoot 'subset-material-symbols-rounded.py'
$glyphJsonPath = Join-Path $temporaryRoot 'material-symbols-rounded-glyphs.json'

$upstreamCommit = '84ccef280841abfac506afc4ad4a2782f6d0a1d0'
$upstreamSha256 = 'C4416E02739ED6865E3218C19DCD62C5A88FB97B8BCC445F24AE8017D11CC2D0'
$codepointsSha256 = '2CEABD8B6EBA5EF81FF6F2FA8A801397F550841BF9CC42B03B5CC2E2D7AEC9F1'
$fontToolsVersion = '4.63.0'
$upstreamUrl = "https://raw.githubusercontent.com/google/material-design-icons/$upstreamCommit/variablefont/MaterialSymbolsRounded%5BFILL,GRAD,opsz,wght%5D.ttf"
$codepointsUrl = "https://raw.githubusercontent.com/google/material-design-icons/$upstreamCommit/variablefont/MaterialSymbolsRounded%5BFILL%2CGRAD%2Copsz%2Cwght%5D.codepoints"

$glyphs = [ordered]@{
    'check_circle' = 0xF0BE
    'close' = 0xE5CD
    'content_paste' = 0xE14F
    'content_paste_off' = 0xE4F8
    'delete' = 0xE92E
    'desktop_windows' = 0xE30C
    'dns' = 0xE875
    'drag_indicator' = 0xE945
    'error' = 0xF8B6
    'expand_more' = 0xE5CF
    'fullscreen' = 0xE5D0
    'fullscreen_exit' = 0xE5D1
    'hourglass_top' = 0xEA5B
    'link_off' = 0xE16F
    'lock' = 0xE899
    'login' = 0xEA77
    'more_horiz' = 0xE5D3
    'open_with' = 0xE89F
    'pending' = 0xEF64
    'person' = 0xF0D3
    'progress_activity' = 0xE9D0
    'remove' = 0xE15B
    'shield_lock' = 0xF686
    'visibility' = 0xE8F4
    'visibility_off' = 0xE8F5
    'volume_off' = 0xE04F
    'volume_up' = 0xE050
}

function Invoke-Python {
    param([string[]]$Arguments)

    & python @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Python command failed with exit code $LASTEXITCODE."
    }
}

New-Item -ItemType Directory -Force -Path $temporaryRoot | Out-Null

if (-not (Test-Path -LiteralPath (Join-Path $fontToolsTarget "fonttools-$fontToolsVersion.dist-info"))) {
    Write-Host "Bootstrapping FontTools $fontToolsVersion into disposable directory $fontToolsTarget"
    Invoke-Python @('-m', 'pip', 'install', '--disable-pip-version-check', '--no-warn-script-location', '--target', $fontToolsTarget, "fonttools==$fontToolsVersion")
}

$existingPythonPath = [Environment]::GetEnvironmentVariable('PYTHONPATH')
$env:PYTHONPATH = if ([string]::IsNullOrEmpty($existingPythonPath)) {
    $fontToolsTarget
} else {
    "$fontToolsTarget$([IO.Path]::PathSeparator)$existingPythonPath"
}

$fontToolsVersionOutput = & python -c 'import fontTools; print(fontTools.__version__)' 2>$null
if ($LASTEXITCODE -ne 0) {
    throw "FontTools $fontToolsVersion is required, and bootstrap into $fontToolsTarget failed."
}
if ($fontToolsVersionOutput.Trim() -ne $fontToolsVersion) {
    throw "FontTools $fontToolsVersion is required, but Python resolved version $($fontToolsVersionOutput.Trim())."
}

if (-not (Test-Path -LiteralPath $upstreamPath)) {
    Write-Host "Downloading pinned Material Symbols Rounded upstream font"
    Invoke-WebRequest -UseBasicParsing -Uri $upstreamUrl -OutFile $upstreamPath
}

$actualUpstreamSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $upstreamPath).Hash
if ($actualUpstreamSha256 -ne $upstreamSha256) {
    throw "Pinned upstream SHA-256 mismatch: expected $upstreamSha256, got $actualUpstreamSha256."
}

if (-not (Test-Path -LiteralPath $codepointsPath)) {
    Write-Host "Downloading pinned Material Symbols Rounded codepoint source"
    Invoke-WebRequest -UseBasicParsing -Uri $codepointsUrl -OutFile $codepointsPath
}

$actualCodepointsSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $codepointsPath).Hash
if ($actualCodepointsSha256 -ne $codepointsSha256) {
    throw "Pinned codepoint source SHA-256 mismatch: expected $codepointsSha256, got $actualCodepointsSha256."
}

$officialCodepoints = @{}
foreach ($line in Get-Content -LiteralPath $codepointsPath) {
    if ($line -match '^(?<name>\S+) (?<codepoint>[0-9a-fA-F]+)$') {
        $officialCodepoints[$Matches.name] = [Convert]::ToInt32($Matches.codepoint, 16)
    }
}
foreach ($glyph in $glyphs.GetEnumerator()) {
    if (-not $officialCodepoints.ContainsKey($glyph.Key)) {
        throw "Pinned official codepoint source does not contain glyph '$($glyph.Key)'."
    }
    if ($officialCodepoints[$glyph.Key] -ne $glyph.Value) {
        throw "Configured codepoint for '$($glyph.Key)' does not match pinned official source."
    }
}

$glyphs | ConvertTo-Json -Compress | Set-Content -LiteralPath $glyphJsonPath -Encoding utf8
$pythonScript = @'
import hashlib
import json
import sys

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

upstream_path, output_path, glyph_json_path = sys.argv[1:]
with open(glyph_json_path, encoding="utf-8-sig") as glyph_file:
    glyphs = json.load(glyph_file)
expected_codepoints = {int(codepoint) for codepoint in glyphs.values()}

font = TTFont(upstream_path, recalcTimestamp=False)
font = instancer.instantiateVariableFont(
    font,
    {"opsz": 24, "wght": 400, "FILL": 0, "GRAD": 0},
    inplace=True,
)

options = subset.Options()
options.glyph_names = True
options.name_IDs = ["*"]
options.name_legacy = True
options.notdef_glyph = True
options.notdef_outline = True
options.recommended_glyphs = True
options.layout_features = ["*"]
subsetter = subset.Subsetter(options=options)
subsetter.populate(unicodes=expected_codepoints)
subsetter.subset(font)
font.recalcTimestamp = False
font.save(output_path)

result = TTFont(output_path, recalcTimestamp=False)
cmap = result.getBestCmap()
missing = [
    f"{name}=U+{codepoint:04X}"
    for name, codepoint in sorted(glyphs.items())
    if int(codepoint) not in cmap
]
if missing:
    raise SystemExit("Subset omitted required codepoints: " + ", ".join(missing))

print("Verified codepoints: " + ", ".join(
    f"{name}=U+{codepoint:04X}" for name, codepoint in sorted(glyphs.items())
))
with open(output_path, "rb") as font_file:
    print("Subset SHA-256: " + hashlib.sha256(font_file.read()).hexdigest().upper())
'@
Set-Content -LiteralPath $pythonScriptPath -Value $pythonScript -Encoding utf8

Invoke-Python @($pythonScriptPath, $upstreamPath, $outputPath, $glyphJsonPath)
