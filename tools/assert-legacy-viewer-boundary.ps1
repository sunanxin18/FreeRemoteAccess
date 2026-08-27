param(
    [string]$ManifestPath = "Cargo.toml"
)

$ErrorActionPreference = "Stop"

$metadataJson = cargo +stable metadata --no-deps --format-version 1 --manifest-path $ManifestPath
if ($LASTEXITCODE -ne 0) {
    throw "cargo metadata failed with exit code $LASTEXITCODE"
}

$metadata = $metadataJson | ConvertFrom-Json

function Get-DirectDependents([string]$dependencyName) {
    @(
        $metadata.packages |
            Where-Object {
                @($_.dependencies | Where-Object { $_.name -eq $dependencyName }).Count -gt 0
            } |
            ForEach-Object { $_.name } |
            Sort-Object -Unique
    )
}

function Assert-ExactDependents([string]$dependencyName, [string[]]$expected) {
    $actual = @(Get-DirectDependents $dependencyName)
    $difference = @(Compare-Object -ReferenceObject $expected -DifferenceObject $actual)
    if ($difference.Count -ne 0) {
        throw "$dependencyName direct dependents must be [$($expected -join ', ')], observed [$($actual -join ', ')]"
    }
}

$legacyLab = "frd-legacy-minifb-lab"
Assert-ExactDependents "minifb" @($legacyLab)
Assert-ExactDependents "cpal" @($legacyLab)

$labDependents = @(Get-DirectDependents $legacyLab)
if ($labDependents.Count -ne 0) {
    throw "$legacyLab must remain a leaf workspace tool; imported by [$($labDependents -join ', ')]"
}

Write-Output "legacy viewer dependency boundary: PASS"
