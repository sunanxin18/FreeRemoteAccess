param(
    [Parameter(Mandatory = $true)]
    [string]$DistDir,
    [switch]$SkipLifecycle
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$DistDir = (Resolve-Path -LiteralPath $DistDir).Path
$ManifestTool = Join-Path $RepoRoot 'packaging\package_manifest.py'
$ManifestPath = Join-Path $DistDir 'artifact-manifest.json'
python $ManifestTool --repo $RepoRoot verify --manifest $ManifestPath
if ($LASTEXITCODE -ne 0) { throw 'artifact_manifest_verify_failed' }
$Manifest = Get-Content -Raw -LiteralPath $ManifestPath | ConvertFrom-Json
$Prefix = "FreeRemoteAccess-$($Manifest.version)-windows-$($Manifest.arch)"
$GuiArtifact = Join-Path $DistDir "$Prefix.exe"
$PortableZip = Join-Path $DistDir "$Prefix-portable.zip"
$MsiArtifact = Join-Path $DistDir "$Prefix.msi"
$LogDir = Join-Path $RepoRoot 'target\package\windows\logs'
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

function Invoke-Msi([string]$Operation, [string]$Msi, [string[]]$Extra, [string]$LogName) {
    $log = Join-Path $LogDir $LogName
    $arguments = "$Operation `"$Msi`" /qn /norestart /L*v `"$log`" $($Extra -join ' ')"
    $process = Start-Process msiexec.exe -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -eq 3010) {
        throw "msi_reboot_required_not_allowed_$LogName"
    }
    if ($process.ExitCode -ne 0) {
        throw "msi_operation_failed_$($process.ExitCode)_$LogName"
    }
}

function Assert-SafeCleanupRoot([string]$Path, [string]$ExpectedRelative) {
    $FullPath = [IO.Path]::GetFullPath($Path)
    if ([IO.Path]::GetRelativePath($RepoRoot, $FullPath) -ne $ExpectedRelative) {
        throw 'package_verify_cleanup_root_invalid'
    }
    if (Test-Path -LiteralPath $FullPath) {
        $Entries = @((Get-Item -Force -LiteralPath $FullPath)) + @(
            Get-ChildItem -Force -LiteralPath $FullPath -Recurse
        )
        if ($Entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) {
            throw 'package_verify_cleanup_reparse_point_rejected'
        }
    }
    return $FullPath
}

function Get-PeSubsystem([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 256 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) {
        throw 'pe_header_invalid'
    }
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
    if ($peOffset -lt 0 -or $peOffset + 94 -gt $bytes.Length) { throw 'pe_header_invalid' }
    if ([BitConverter]::ToUInt32($bytes, $peOffset) -ne 0x00004550) { throw 'pe_signature_invalid' }
    return [BitConverter]::ToUInt16($bytes, $peOffset + 24 + 68)
}

if (-not ('NativeResourceProbe' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NativeResourceProbe {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll")]
    public static extern bool FreeLibrary(IntPtr module);
}
'@
}

function Assert-PeResources([string]$Path) {
    $module = [NativeResourceProbe]::LoadLibraryEx($Path, [IntPtr]::Zero, 0x00000002)
    if ($module -eq [IntPtr]::Zero) { throw 'pe_resource_load_failed' }
    try {
        foreach ($type in @(14, 16, 24)) {
            if ([NativeResourceProbe]::FindResource($module, [IntPtr]::new(1), [IntPtr]::new($type)) -eq [IntPtr]::Zero) {
                throw "pe_resource_missing_$type"
            }
        }
    } finally {
        [void][NativeResourceProbe]::FreeLibrary($module)
    }
}

function Assert-SupportBundle([string]$Root) {
    $fdkRoot = Join-Path $Root "THIRD_PARTY\$($Manifest.fdk_aac.package)-$($Manifest.fdk_aac.version)"
    $notice = Join-Path $fdkRoot 'aac\NOTICE'
    $archive = Join-Path $fdkRoot "source\$($Manifest.fdk_aac.package)-$($Manifest.fdk_aac.version).crate"
    if (!(Test-Path -LiteralPath $notice -PathType Leaf)) { throw 'package_fdk_notice_missing' }
    if (!(Test-Path -LiteralPath $archive -PathType Leaf)) { throw 'package_fdk_source_missing' }
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $Manifest.fdk_aac.crate_sha256) { throw 'package_fdk_source_hash_mismatch' }
    $canonicalNotice = Join-Path $DistDir $Manifest.fdk_aac.notice_path
    if (![Linq.Enumerable]::SequenceEqual([IO.File]::ReadAllBytes($notice), [IO.File]::ReadAllBytes($canonicalNotice))) {
        throw 'package_fdk_notice_mismatch'
    }
}

function Assert-VersionInfo([string]$Path) {
    $versionInfo = (Get-Item -LiteralPath $Path).VersionInfo
    if ($versionInfo.ProductName -ne 'FreeRemoteAccess') { throw 'pe_product_name_invalid' }
    foreach ($value in @($versionInfo.FileVersion, $versionInfo.ProductVersion)) {
        if ($value -notmatch '^([0-9]+)\.([0-9]+)\.([0-9]+)$') {
            throw 'pe_version_resource_invalid'
        }
        if ("$($Matches[1]).$($Matches[2]).$($Matches[3])" -ne $Manifest.version) {
            throw 'pe_version_resource_mismatch'
        }
    }
}

function Assert-ExecutableSet(
    [string]$Root,
    [string]$ExpectedGuiHash,
    [string]$ExpectedCliHash
) {
    $gui = Join-Path $Root 'FreeRemoteAccess.exe'
    $cli = Join-Path $Root 'freeremotedesk-cli.exe'
    if (!(Test-Path -LiteralPath $gui -PathType Leaf)) { throw 'packaged_gui_missing' }
    if (!(Test-Path -LiteralPath $cli -PathType Leaf)) { throw 'packaged_cli_missing' }
    if ((Get-FileHash -LiteralPath $gui -Algorithm SHA256).Hash -ne $ExpectedGuiHash) {
        throw 'packaged_gui_hash_mismatch'
    }
    if ((Get-FileHash -LiteralPath $cli -Algorithm SHA256).Hash -ne $ExpectedCliHash) {
        throw 'packaged_cli_hash_mismatch'
    }
    if ((Get-PeSubsystem $gui) -ne 2) { throw 'gui_pe_subsystem_must_be_windows' }
    if ((Get-PeSubsystem $cli) -ne 3) { throw 'cli_pe_subsystem_must_be_console' }
    Assert-PeResources $gui
    Assert-PeResources $cli
    Assert-VersionInfo $gui
    Assert-VersionInfo $cli
    & $cli --help *> $null
    if ($LASTEXITCODE -ne 0) { throw 'cli_help_failed' }
}

if ((Get-PeSubsystem $GuiArtifact) -ne 2) { throw 'gui_pe_subsystem_must_be_windows' }
Assert-PeResources $GuiArtifact
Assert-VersionInfo $GuiArtifact
$cliArtifact = Join-Path $RepoRoot 'target\release\freeremotedesk.exe'
if ((Get-PeSubsystem $cliArtifact) -ne 3) { throw 'cli_pe_subsystem_must_be_console' }
Assert-PeResources $cliArtifact
Assert-VersionInfo $cliArtifact
& $cliArtifact --help *> $null
if ($LASTEXITCODE -ne 0) { throw 'cli_help_failed' }
$ExpectedGuiHash = (Get-FileHash -LiteralPath $GuiArtifact -Algorithm SHA256).Hash
$ExpectedCliHash = (Get-FileHash -LiteralPath $cliArtifact -Algorithm SHA256).Hash

$ExtractRoot = Assert-SafeCleanupRoot (Join-Path $RepoRoot 'target\package\windows\verify') 'target\package\windows\verify'
if (Test-Path -LiteralPath $ExtractRoot) { Remove-Item -LiteralPath $ExtractRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $ExtractRoot | Out-Null
$InstalledMsi = $null
$Gui = $null
$InstallRoot = Join-Path $env:ProgramFiles 'FreeRemoteAccess'
$Shortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\FreeRemoteAccess.lnk'
$PrimaryError = $null
$CleanupErrors = [Collections.Generic.List[string]]::new()

function Assert-LifecycleRemoved {
    if (Test-Path -LiteralPath $Shortcut) { throw 'msi_shortcut_remained_after_uninstall' }
    if (Test-Path -LiteralPath $InstallRoot) { throw 'msi_install_directory_remained_after_uninstall' }
    $remaining = Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
        Where-Object DisplayName -eq 'FreeRemoteAccess'
    if ($remaining) { throw 'msi_registration_remained_after_uninstall' }
}

try {
    $zipRoot = Join-Path $ExtractRoot 'zip'
    Expand-Archive -LiteralPath $PortableZip -DestinationPath $zipRoot
    $portable = Join-Path $zipRoot 'FreeRemoteAccess'
    Assert-ExecutableSet $portable $ExpectedGuiHash $ExpectedCliHash
    Assert-SupportBundle $portable

    $adminRoot = Join-Path $ExtractRoot 'msi-admin'
    Invoke-Msi '/a' $MsiArtifact @("TARGETDIR=`"$adminRoot`"") 'administrative-extract.log'
    $installedRoot = Join-Path $adminRoot 'Program Files\FreeRemoteAccess'
    Assert-ExecutableSet $installedRoot $ExpectedGuiHash $ExpectedCliHash
    Assert-SupportBundle $installedRoot

    if (!$SkipLifecycle) {
        $MsiVersion = (& python $ManifestTool --repo $RepoRoot msi-version).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'msi_product_version_failed' }
        $parts = $MsiVersion.Split('.') | ForEach-Object { [int]$_ }
        if ($parts[2] -gt 0) { $parts[2]-- }
        elseif ($parts[1] -gt 0) { $parts[1]--; $parts[2] = 65535 }
        elseif ($parts[0] -gt 0) { $parts[0]--; $parts[1] = 255; $parts[2] = 65535 }
        else { throw 'msi_upgrade_fixture_version_unavailable' }
        $PreviousVersion = $parts -join '.'
        $MsiRoot = Join-Path $RepoRoot 'target\package\windows\msi-root'
        $FdkRoot = Get-ChildItem -LiteralPath (Join-Path $MsiRoot 'THIRD_PARTY') -Directory
        if (@($FdkRoot).Count -ne 1) { throw 'msi_fdk_directory_ambiguous' }
        $FdkArchive = Get-ChildItem -LiteralPath (Join-Path $FdkRoot.FullName 'source') -Filter '*.crate' -File
        if (@($FdkArchive).Count -ne 1) { throw 'msi_fdk_archive_ambiguous' }
        $PreviousMsi = Join-Path $ExtractRoot 'previous.msi'
        wix build -arch x64 -d "SourceDir=$MsiRoot" -d "PackageVersion=$PreviousVersion" `
            -d "FdkDirectoryName=$($FdkRoot.Name)" -d "FdkArchiveName=$($FdkArchive.Name)" `
            -pdbtype none -o $PreviousMsi (Join-Path $PSScriptRoot 'wix\main.wxs')
        if ($LASTEXITCODE -ne 0) { throw 'msi_upgrade_fixture_build_failed' }

        $existing = Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
            Where-Object DisplayName -eq 'FreeRemoteAccess'
        if ($existing) { throw 'msi_lifecycle_requires_clean_runner' }
        $InstalledMsi = $PreviousMsi
        Invoke-Msi '/i' $PreviousMsi @() 'install-previous.log'
        $InstalledMsi = $MsiArtifact
        Invoke-Msi '/i' $MsiArtifact @() 'upgrade-current.log'
        $product = @(Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
            Where-Object DisplayName -eq 'FreeRemoteAccess')
        if ($product.Count -ne 1 -or $product[0].DisplayVersion -ne $MsiVersion) {
            throw 'msi_major_upgrade_failed'
        }
        Assert-ExecutableSet $InstallRoot $ExpectedGuiHash $ExpectedCliHash
        Assert-SupportBundle $InstallRoot
        if (!(Test-Path -LiteralPath $Shortcut)) { throw 'msi_start_menu_shortcut_missing' }
        $ShortcutInfo = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
        if ($ShortcutInfo.TargetPath -ne (Join-Path $InstallRoot 'FreeRemoteAccess.exe')) {
            throw 'msi_start_menu_target_invalid'
        }
        Start-Process -FilePath $Shortcut | Out-Null
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            Start-Sleep -Milliseconds 250
            $Gui = Get-Process -Name 'FreeRemoteAccess' -ErrorAction SilentlyContinue |
                Where-Object Path -eq $ShortcutInfo.TargetPath |
                Select-Object -First 1
        } while (($null -eq $Gui -or $Gui.MainWindowHandle -eq 0) -and [DateTime]::UtcNow -lt $deadline)
        if ($null -eq $Gui -or $Gui.HasExited -or $Gui.MainWindowHandle -eq 0) { throw 'msi_gui_window_not_alive' }
        Stop-Process -Id $Gui.Id -Force
        $Gui.WaitForExit()
        $Gui = $null

        Invoke-Msi '/x' $MsiArtifact @() 'uninstall-current.log'
        Assert-LifecycleRemoved
        $InstalledMsi = $null
    }
} catch {
    $PrimaryError = $_
} finally {
    if ($null -ne $Gui -and !$Gui.HasExited) {
        try {
            Stop-Process -Id $Gui.Id -Force
            $Gui.WaitForExit()
        } catch {
            $CleanupErrors.Add("gui_cleanup_failed:$($_.Exception.Message)")
        }
    }
    if ($null -ne $InstalledMsi) {
        try {
            Invoke-Msi '/x' $InstalledMsi @() 'cleanup-after-failure.log'
            Assert-LifecycleRemoved
        } catch {
            $CleanupErrors.Add("msi_cleanup_failed:$($_.Exception.Message)")
        }
    }
    try {
        $ExtractRoot = Assert-SafeCleanupRoot $ExtractRoot 'target\package\windows\verify'
        if (Test-Path -LiteralPath $ExtractRoot) { Remove-Item -LiteralPath $ExtractRoot -Recurse -Force }
    } catch {
        $CleanupErrors.Add("verify_temp_cleanup_failed:$($_.Exception.Message)")
    }
}

if ($null -ne $PrimaryError) {
    foreach ($cleanup in $CleanupErrors) { Write-Warning $cleanup }
    throw $PrimaryError
}
if ($CleanupErrors.Count -gt 0) { throw ($CleanupErrors -join ';') }

Write-Output 'windows-package-verification: ok'
