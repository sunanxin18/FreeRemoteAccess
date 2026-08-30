[CmdletBinding(DefaultParameterSetName='Capture')]
param(
  [Parameter(Mandatory=$true, ParameterSetName='Capture')][ValidateSet('serial','candidate')][string]$Implementation,
  [Parameter(Mandatory=$true, ParameterSetName='Capture')][ValidatePattern('^[A-Za-z0-9_-]{1,64}$')][string]$RunId,
  [Parameter(ParameterSetName='Capture')][string]$OutputDirectory = '.\target\validation',
  [Parameter(ParameterSetName='Capture')][switch]$AutoConnect,
  [Parameter(ParameterSetName='Capture')][ValidateSet('macos','windows','linux','custom')][string]$AutoConnectTarget,
  [Parameter(ParameterSetName='Capture')][ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._:-]{0,252}$')][string]$AutoConnectAddress,
  [Parameter(ParameterSetName='Capture')][UInt16]$AutoConnectPort,
  [Parameter(ParameterSetName='Capture')][ValidatePattern('^[a-z0-9-]{1,64}$')][string]$AutoConnectProtocol,
  [Parameter(Mandatory=$true, ParameterSetName='SelfTest')][switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ProcessHeader = 'schema_version,run_id,implementation,phase,second,monotonic_us,process_cpu_total_us,process_cpu_delta_us,working_set_bytes'
$DevicePrefixes = @('\\.\', '\\?\', '\??\')

function Assert-True([bool]$Condition, [string]$Code) {
  if (-not $Condition) { throw $Code }
}

function Get-CaptureClientArgumentVector(
  [bool]$AutoConnect,
  [string]$Target,
  [string]$Address,
  [UInt16]$Port,
  [string]$Protocol
) {
  if (-not $AutoConnect) { return @() }

  $validTargets = @('macos', 'windows', 'linux', 'custom')
  if ([string]::IsNullOrWhiteSpace($Target) -or
    $Target -notin $validTargets -or
    [string]::IsNullOrWhiteSpace($Address) -or
    $Address -notmatch '^[A-Za-z0-9][A-Za-z0-9._:-]{0,252}$' -or
    $Port -eq 0 -or
    [string]::IsNullOrWhiteSpace($Protocol) -or
    $Protocol -notmatch '^[a-z0-9-]{1,64}$') {
    throw 'auto_connect_configuration_incomplete'
  }

  if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable('FRD_USERNAME', 'Process'))) {
    throw 'auto_connect_username_environment_missing'
  }
  if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable('FRD_PASSWORD', 'Process'))) {
    throw 'auto_connect_password_environment_missing'
  }

  return @(
    '--target', $Target,
    '--address', $Address,
    '--port', $Port.ToString([Globalization.CultureInfo]::InvariantCulture),
    '--protocol', $Protocol,
    '--username-provider', 'environment',
    '--password-provider', 'environment',
    '--connect'
  )
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
    [void][IO.Directory]::CreateDirectory($missing[$index])
    Assert-NormalDirectory $missing[$index]
  }
}

function Resolve-SafeOutputDirectory([string]$RepositoryRoot, [string]$RequestedDirectory) {
  Assert-NoDeviceNamespace $RequestedDirectory
  $outputFullPath = if ([IO.Path]::IsPathRooted($RequestedDirectory)) {
    [IO.Path]::GetFullPath($RequestedDirectory)
  } else {
    [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $RequestedDirectory))
  }
  $validationRoot = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot 'target\validation'))
  New-SafeDirectory $outputFullPath $validationRoot $RepositoryRoot
  return $outputFullPath
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

function Assert-Sequence($Actual, [string[]]$Expected, [string]$Code) {
  Assert-True ($Actual.Count -eq $Expected.Count) $Code
  for ($index = 0; $index -lt $Expected.Count; $index++) {
    Assert-True ([string]$Actual[$index] -ceq $Expected[$index]) $Code
  }
}

function Invoke-CleanupWorkflow(
  [scriptblock]$HasExited,
  [scriptblock]$RequestNormalClose,
  [scriptblock]$WaitAfterClose,
  [scriptblock]$TerminateExactProcess,
  [scriptblock]$WaitAfterTerminate
) {
  $actions = [Collections.Generic.List[string]]::new()
  if ([bool](& $HasExited)) {
    $actions.Add('ConfirmExit')
    return $actions.ToArray()
  }

  $actions.Add('RequestNormalClose')
  [void](& $RequestNormalClose)
  $actions.Add('BoundedWaitAfterClose')
  if ([bool](& $WaitAfterClose)) {
    $actions.Add('ConfirmExit')
    return $actions.ToArray()
  }

  $actions.Add('TerminateExactProcess')
  [void](& $TerminateExactProcess)
  $actions.Add('BoundedWaitAfterTerminate')
  Assert-True ([bool](& $WaitAfterTerminate)) 'client_cleanup_termination_timeout'
  $actions.Add('ConfirmExit')
  return $actions.ToArray()
}

function Stop-StartedClientOnFailure([Diagnostics.Process]$Process) {
  $startedProcessId = $Process.Id
  [void](Invoke-CleanupWorkflow `
    {
      $Process.Refresh()
      $Process.HasExited
    } `
    {
      $Process.Refresh()
      Assert-True ($Process.Id -eq $startedProcessId) 'client_cleanup_process_identity_changed'
      [void]$Process.CloseMainWindow()
    } `
    { $Process.WaitForExit(5000) } `
    {
      $Process.Refresh()
      if (-not $Process.HasExited) {
        Assert-True ($Process.Id -eq $startedProcessId) 'client_cleanup_process_identity_changed'
        $Process.Kill()
      }
    } `
    { $Process.WaitForExit(5000) })
  $Process.Refresh()
  Assert-True ($Process.HasExited) 'client_cleanup_exit_unconfirmed'
}

function Finalize-CaptureArtifacts([string[]]$Paths, [bool]$CaptureComplete) {
  if ($CaptureComplete) { return }
  foreach ($path in $Paths) {
    Assert-True (-not [string]::IsNullOrWhiteSpace($path)) 'capture_artifact_path_missing'
    Assert-True (-not [IO.Directory]::Exists($path)) 'capture_artifact_cleanup_failed'
    if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    Assert-True (-not [IO.File]::Exists($path)) 'capture_artifact_cleanup_failed'
  }
}

function Invoke-SelfTest {
  $testRepositoryRoot = Join-Path ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\target'))) ".run-frame-metrics-selftest-$([Guid]::NewGuid().ToString('N'))"
  $defaultOutput = [IO.Path]::GetFullPath((Join-Path $testRepositoryRoot 'target\validation'))
  $credentialNames = @('FRD_USERNAME', 'FRD_PASSWORD')
  $savedCredentials = @{}
  foreach ($name in $credentialNames) {
    $savedCredentials[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
  }

  function Assert-ThrowsCode([scriptblock]$Action, [string]$ExpectedCode, [string]$FailureCode) {
    $threw = $false
    try {
      [void](& $Action)
    } catch {
      $threw = $true
      Assert-True ($_.Exception.Message -ceq $ExpectedCode) $FailureCode
    }
    Assert-True $threw $FailureCode
  }

  try {
    [void][IO.Directory]::CreateDirectory($testRepositoryRoot)
    $resolvedDefault = Resolve-SafeOutputDirectory $testRepositoryRoot '.\target\validation'
    Assert-True ($resolvedDefault -ceq $defaultOutput) 'selftest_default_directory_resolution'
    Assert-True (Test-Path -LiteralPath $defaultOutput -PathType Container) 'selftest_default_directory_not_created'

    [Environment]::SetEnvironmentVariable('FRD_USERNAME', 'selftest-username-material', 'Process')
    [Environment]::SetEnvironmentVariable('FRD_PASSWORD', 'selftest-password-material', 'Process')

    $disabledClientArguments = @(Get-CaptureClientArgumentVector -AutoConnect:$false -Target 'macos' -Address 'capture.example.test' -Port 5900 -Protocol 'hpss')
    Assert-True ($disabledClientArguments.Count -eq 0) 'selftest_autoconnect_disabled_arguments'

    $enabledClientArguments = @(Get-CaptureClientArgumentVector -AutoConnect:$true -Target 'macos' -Address 'capture.example.test' -Port 5900 -Protocol 'hpss')
    Assert-Sequence $enabledClientArguments @('--target', 'macos', '--address', 'capture.example.test', '--port', '5900', '--protocol', 'hpss', '--username-provider', 'environment', '--password-provider', 'environment', '--connect') 'selftest_autoconnect_enabled_arguments'
    Assert-True (-not ($enabledClientArguments -contains 'selftest-username-material')) 'selftest_autoconnect_username_material_exposed'
    Assert-True (-not ($enabledClientArguments -contains 'selftest-password-material')) 'selftest_autoconnect_password_material_exposed'

    foreach ($missingConfiguration in @(
      @{ Target = ''; Address = 'capture.example.test'; Port = 5900; Protocol = 'hpss' },
      @{ Target = 'macos'; Address = ''; Port = 5900; Protocol = 'hpss' },
      @{ Target = 'macos'; Address = 'capture.example.test'; Port = 0; Protocol = 'hpss' },
      @{ Target = 'macos'; Address = 'capture.example.test'; Port = 5900; Protocol = '' }
    )) {
      Assert-ThrowsCode { Get-CaptureClientArgumentVector -AutoConnect:$true -Target $missingConfiguration.Target -Address $missingConfiguration.Address -Port $missingConfiguration.Port -Protocol $missingConfiguration.Protocol } 'auto_connect_configuration_incomplete' 'selftest_autoconnect_incomplete_configuration'
    }

    [Environment]::SetEnvironmentVariable('FRD_USERNAME', $null, 'Process')
    Assert-ThrowsCode { Get-CaptureClientArgumentVector -AutoConnect:$true -Target 'macos' -Address 'capture.example.test' -Port 5900 -Protocol 'hpss' } 'auto_connect_username_environment_missing' 'selftest_autoconnect_username_environment'

    [Environment]::SetEnvironmentVariable('FRD_USERNAME', 'selftest-username-material', 'Process')
    [Environment]::SetEnvironmentVariable('FRD_PASSWORD', $null, 'Process')
    Assert-ThrowsCode { Get-CaptureClientArgumentVector -AutoConnect:$true -Target 'macos' -Address 'capture.example.test' -Port 5900 -Protocol 'hpss' } 'auto_connect_password_environment_missing' 'selftest_autoconnect_password_environment'

    [Environment]::SetEnvironmentVariable('FRD_PASSWORD', 'selftest-password-material', 'Process')

    $normalCalls = [Collections.Generic.List[string]]::new()
    $normalActions = @(Invoke-CleanupWorkflow `
      { $false } `
      { [void]$normalCalls.Add('request_normal_close') } `
      { [void]$normalCalls.Add('wait_after_close'); $true } `
      { throw 'selftest_normal_path_terminated' } `
      { throw 'selftest_normal_path_waited_after_terminate' })
    Assert-Sequence $normalCalls @('request_normal_close', 'wait_after_close') 'selftest_normal_cleanup_calls'
    Assert-Sequence $normalActions @('RequestNormalClose', 'BoundedWaitAfterClose', 'ConfirmExit') 'selftest_normal_cleanup_actions'

    $forcedCalls = [Collections.Generic.List[string]]::new()
    $forcedActions = @(Invoke-CleanupWorkflow `
      { $false } `
      { [void]$forcedCalls.Add('request_normal_close') } `
      { [void]$forcedCalls.Add('wait_after_close'); $false } `
      { [void]$forcedCalls.Add('terminate_exact_process') } `
      { [void]$forcedCalls.Add('wait_after_terminate'); $true })
    Assert-Sequence $forcedCalls @('request_normal_close', 'wait_after_close', 'terminate_exact_process', 'wait_after_terminate') 'selftest_forced_cleanup_calls'
    Assert-Sequence $forcedActions @('RequestNormalClose', 'BoundedWaitAfterClose', 'TerminateExactProcess', 'BoundedWaitAfterTerminate', 'ConfirmExit') 'selftest_forced_cleanup_actions'

    $incompleteEvent = Join-Path $defaultOutput 'incomplete-events.csv'
    $incompleteProcess = Join-Path $defaultOutput 'incomplete-process.csv'
    [IO.File]::WriteAllText($incompleteEvent, 'incomplete')
    [IO.File]::WriteAllText($incompleteProcess, 'incomplete')
    Finalize-CaptureArtifacts @($incompleteEvent, $incompleteProcess) $false
    Assert-True (-not [IO.File]::Exists($incompleteEvent) -and -not [IO.File]::Exists($incompleteProcess)) 'selftest_incomplete_artifacts_retained'

    [IO.File]::WriteAllText($incompleteEvent, 'complete')
    [IO.File]::WriteAllText($incompleteProcess, 'complete')
    Finalize-CaptureArtifacts @($incompleteEvent, $incompleteProcess) $true
    Assert-True ([IO.File]::Exists($incompleteEvent) -and [IO.File]::Exists($incompleteProcess)) 'selftest_complete_artifacts_removed'
  } finally {
    foreach ($name in $credentialNames) {
      [Environment]::SetEnvironmentVariable($name, $savedCredentials[$name], 'Process')
    }
    if (Test-Path -LiteralPath $testRepositoryRoot) {
      [IO.Directory]::Delete($testRepositoryRoot, $true)
    }
  }
  Write-Output 'SELFTEST PASS'
}

if ($SelfTest) {
  Invoke-SelfTest
  exit 0
}

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$outputFullPath = Resolve-SafeOutputDirectory $repositoryRoot $OutputDirectory

$existingClients = @(Get-Process -Name 'freeremotedesk-windows' -ErrorAction SilentlyContinue)
Assert-True ($existingClients.Count -eq 0) 'existing_client_detected'

$clientPath = Join-Path $repositoryRoot 'target\release\freeremotedesk-windows.exe'
Assert-True (Test-Path -LiteralPath $clientPath -PathType Leaf) 'release_client_missing'
$clientArguments = @(Get-CaptureClientArgumentVector -AutoConnect:$AutoConnect -Target $AutoConnectTarget -Address $AutoConnectAddress -Port $AutoConnectPort -Protocol $AutoConnectProtocol)
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
$captureComplete = $false
try {
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_PATH', $eventPath, 'Process')
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_RUN_ID', $RunId, 'Process')
  [Environment]::SetEnvironmentVariable('FRD_FRAME_METRICS_IMPLEMENTATION', $Implementation, 'Process')

  $processStream = [IO.File]::Open($processPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
  $processWriter = [IO.StreamWriter]::new($processStream, [Text.UTF8Encoding]::new($false))
  $processWriter.WriteLine($ProcessHeader)
  $processWriter.Flush()

  $client = if ($clientArguments.Count -eq 0) {
    Start-Process -FilePath $clientPath -PassThru
  } else {
    Start-Process -FilePath $clientPath -ArgumentList $clientArguments -PassThru
  }
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
  $captureComplete = $true
} finally {
  $finalizationError = $null
  if (-not $captureComplete -and $null -ne $client) {
    try { Stop-StartedClientOnFailure $client } catch { $finalizationError = $_ }
  }
  try {
    if ($null -ne $processWriter) { $processWriter.Dispose() }
    elseif ($null -ne $processStream) { $processStream.Dispose() }
  } catch {
    if ($null -eq $finalizationError) { $finalizationError = $_ }
  }
  foreach ($name in $names) {
    [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
  }
  try {
    Finalize-CaptureArtifacts @($eventPath, $processPath) $captureComplete
  } catch {
    if ($null -eq $finalizationError) { $finalizationError = $_ }
  }
  if ($null -ne $finalizationError) { throw $finalizationError }
}

Write-Output $eventPath
Write-Output $processPath
