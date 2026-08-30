param(
  [Parameter(Mandatory=$true)][ValidateSet('serial','candidate')][string]$Implementation,
  [Parameter(Mandatory=$true)][ValidatePattern('^[A-Za-z0-9_-]{1,64}$')][string]$RunId,
  [string]$OutputDirectory = '.\target\validation'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ProcessHeader = 'schema_version,run_id,implementation,phase,second,monotonic_us,process_cpu_total_us,process_cpu_delta_us,working_set_bytes'
$DevicePrefixes = @('\\.\', '\\?\', '\??\')

function Assert-True([bool]$Condition, [string]$Code) {
  if (-not $Condition) { throw $Code }
}

function Test-Within([string]$Path, [string]$Root) {
  $comparison = [StringComparison]::OrdinalIgnoreCase
  return $Path.Equals($Root, $comparison) -or
    $Path.StartsWith($Root.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar, $comparison)
}

function Assert-NoDeviceNamespace([string]$Path) {
  $normalized = $Path.Replace('/', '\')
  foreach ($prefix in $DevicePrefixes) {
    Assert-True (-not $normalized.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) 'unsafe_device_namespace'
  }
}

function Assert-NormalDirectory([string]$Path) {
  Assert-True (Test-Path -LiteralPath $Path -PathType Container) 'metrics_directory_missing'
  $item = Get-Item -LiteralPath $Path -Force
  Assert-True (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'metrics_directory_reparse'
}

function New-SafeDirectory([string]$Path, [string]$AllowedRoot, [string]$RepositoryRoot) {
  Assert-True (Test-Within $Path $AllowedRoot) 'metrics_directory_out_of_tree'
  $cursor = $Path
  $missing = [Collections.Generic.List[string]]::new()
  while (-not (Test-Path -LiteralPath $cursor)) {
    $missing.Add($cursor)
    $parent = Split-Path -Parent $cursor
    Assert-True (-not [string]::IsNullOrWhiteSpace($parent) -and $parent -ne $cursor) 'metrics_directory_parent_missing'
    $cursor = $parent
  }
  Assert-True (Test-Within $cursor $RepositoryRoot) 'metrics_directory_escaped_repository'
  Assert-NormalDirectory $cursor
  $ancestor = $cursor
  while (Test-Within $ancestor $RepositoryRoot) {
    Assert-NormalDirectory $ancestor
    if ($ancestor.Equals($RepositoryRoot, [StringComparison]::OrdinalIgnoreCase)) { break }
    $ancestor = Split-Path -Parent $ancestor
  }
  for ($index = $missing.Count - 1; $index -ge 0; $index--) {
    [void](New-Item -ItemType Directory -LiteralPath $missing[$index])
    Assert-NormalDirectory $missing[$index]
  }
}

function Read-EventRows([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return @() }
  try { return @(Import-Csv -LiteralPath $Path) } catch { return @() }
}

function Wait-EventRow(
  [string]$Path,
  [string]$Phase,
  [string]$Event,
  [int]$TimeoutSeconds
) {
  $deadline = [Diagnostics.Stopwatch]::StartNew()
  while ($deadline.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    $match = @(Read-EventRows $Path | Where-Object {
      $_.phase -eq $Phase -and $_.event -eq $Event
    } | Select-Object -Last 1)
    if ($match.Count -eq 1) { return $match[0] }
    Start-Sleep -Milliseconds 50
  }
  throw "metric_marker_timeout_${Phase}_${Event}"
}

function Get-ProcessCpuUs([Diagnostics.Process]$Process) {
  $Process.Refresh()
  Assert-True (-not $Process.HasExited) 'client_exited_during_capture'
  return [UInt64][Math]::Floor($Process.TotalProcessorTime.Ticks / 10)
}

function Write-PhaseSamples(
  [Diagnostics.Process]$Process,
  [IO.StreamWriter]$Writer,
  [string]$Phase,
  [UInt64]$PhaseOriginUs
) {
  $stopwatch = [Diagnostics.Stopwatch]::StartNew()
  $previousCpu = $null
  foreach ($second in 0..30) {
    $deadlineTicks = [Int64]($second * [Diagnostics.Stopwatch]::Frequency)
    while ($stopwatch.ElapsedTicks -lt $deadlineTicks) {
      $remainingTicks = $deadlineTicks - $stopwatch.ElapsedTicks
      $remainingMs = [Math]::Floor(($remainingTicks * 1000.0) / [Diagnostics.Stopwatch]::Frequency)
      Start-Sleep -Milliseconds ([int][Math]::Max(1, [Math]::Min(20, $remainingMs)))
    }
    $cpu = Get-ProcessCpuUs $Process
    $Process.Refresh()
    $workingSet = [UInt64]$Process.WorkingSet64
    $monotonicUs = $PhaseOriginUs + [UInt64][Math]::Floor(($stopwatch.ElapsedTicks * 1000000.0) / [Diagnostics.Stopwatch]::Frequency)
    $delta = if ($null -eq $previousCpu) { '' } else {
      Assert-True ($cpu -ge [UInt64]$previousCpu) 'non_monotonic_process_cpu'
      [string]($cpu - [UInt64]$previousCpu)
    }
    $Writer.WriteLine("1,$RunId,$Implementation,$Phase,$second,$monotonicUs,$cpu,$delta,$workingSet")
    $Writer.Flush()
    $previousCpu = $cpu
  }
}

function Get-MainWindowHandle([Diagnostics.Process]$Process, [int]$TimeoutSeconds) {
  $stopwatch = [Diagnostics.Stopwatch]::StartNew()
  while ($stopwatch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    $Process.Refresh()
    Assert-True (-not $Process.HasExited) 'client_exited_before_window_ready'
    if ($Process.MainWindowHandle -ne [IntPtr]::Zero) { return $Process.MainWindowHandle }
    Start-Sleep -Milliseconds 50
  }
  throw 'client_main_window_unavailable'
}

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
Assert-NoDeviceNamespace $OutputDirectory
$outputFullPath = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
  [IO.Path]::GetFullPath($OutputDirectory)
} else {
  [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputDirectory))
}
$validationRoot = [IO.Path]::GetFullPath((Join-Path $repositoryRoot 'target\validation'))
New-SafeDirectory $outputFullPath $validationRoot $repositoryRoot

$existingClients = @(Get-Process -Name 'freeremotedesk-windows' -ErrorAction SilentlyContinue)
Assert-True ($existingClients.Count -eq 0) 'existing_client_detected'

$clientPath = Join-Path $repositoryRoot 'target\release\freeremotedesk-windows.exe'
Assert-True (Test-Path -LiteralPath $clientPath -PathType Leaf) 'release_client_missing'
$eventPath = Join-Path $outputFullPath "$RunId-$Implementation-events.csv"
$processPath = Join-Path $outputFullPath "$RunId-$Implementation-process.csv"
Assert-True (-not (Test-Path -LiteralPath $eventPath)) 'event_output_exists'
Assert-True (-not (Test-Path -LiteralPath $processPath)) 'process_output_exists'

if (-not ('FrdMetricWindowControl' -as [type])) {
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FrdMetricWindowControl {
  [DllImport("user32.dll")]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);
}
'@
}

$names = @('FRD_FRAME_METRICS_PATH', 'FRD_FRAME_METRICS_RUN_ID', 'FRD_FRAME_METRICS_IMPLEMENTATION')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$processStream = $null
$processWriter = $null
$client = $null
try {
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_PATH', $eventPath, 'Process')
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_RUN_ID', $RunId, 'Process')
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_IMPLEMENTATION', $Implementation, 'Process')

  $processStream = [IO.File]::Open($processPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
  $processWriter = [IO.StreamWriter]::new($processStream, [Text.UTF8Encoding]::new($false))
  $processWriter.WriteLine($ProcessHeader)
  $processWriter.Flush()

  $client = Start-Process -FilePath $clientPath -PassThru
  [void](Wait-EventRow $eventPath 'VisibleWarmup' 'PhaseBoundary' 180)
  $visible = Wait-EventRow $eventPath 'VisibleMeasurement' 'PhaseBoundary' 15
  Write-PhaseSamples $client $processWriter 'VisibleMeasurement' ([UInt64]$visible.monotonic_us)

  $handle = Get-MainWindowHandle $client 15
  Assert-True ([FrdMetricWindowControl]::ShowWindowAsync($handle, 6)) 'client_minimize_failed'
  [void](Wait-EventRow $eventPath 'MinimizedWarmup' 'PhaseBoundary' 15)
  $minimized = Wait-EventRow $eventPath 'MinimizedMeasurement' 'PhaseBoundary' 15
  Write-PhaseSamples $client $processWriter 'MinimizedMeasurement' ([UInt64]$minimized.monotonic_us)

  Assert-True ([FrdMetricWindowControl]::ShowWindowAsync($handle, 9)) 'client_restore_failed'
  [void](Wait-EventRow $eventPath 'Restore' 'PhaseBoundary' 15)
  [void](Wait-EventRow $eventPath 'Restore' 'Presentation' 30)

  Write-Host '采样完成。请在客户端中正常断开并关闭窗口。'
  Assert-True ($client.WaitForExit(600000)) 'client_normal_close_timeout'
  Assert-True ($client.ExitCode -eq 0) 'client_exit_nonzero'
  $faults = @(Read-EventRows $eventPath | Where-Object { $_.event -eq 'StableFault' })
  Assert-True ($faults.Count -eq 0) 'stable_fault_present'
} finally {
  if ($null -ne $processWriter) { $processWriter.Dispose() }
  elseif ($null -ne $processStream) { $processStream.Dispose() }
  foreach ($name in $names) {
    [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
  }
}

Write-Output $eventPath
Write-Output $processPath
