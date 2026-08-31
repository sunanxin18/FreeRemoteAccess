param(
  [string]$SerialEvents,
  [string]$SerialProcessSamples,
  [string]$CandidateEvents,
  [string]$CandidateProcessSamples,
  [string]$OutputPath,
  [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$EventHeader = 'schema_version,run_id,implementation,phase,event,batch_result,batch_failure_class,monotonic_us,session_id,generation,revision,source_updates,transactions,rectangles,batch_cpu_us,mailbox_age_us,scope_begins,scope_finishes,scope_polls,gpu_fault_code,process_cpu_total_us,process_cpu_delta_us,working_set_bytes,frame_response_ms,input_to_next_present_us'
$ProcessHeader = 'schema_version,run_id,implementation,phase,second,monotonic_us,process_cpu_total_us,process_cpu_delta_us,working_set_bytes'
$MeasuredPhases = @('VisibleMeasurement', 'MinimizedMeasurement')

function Assert-True([bool]$Condition, [string]$Code) {
  if (-not $Condition) { throw $Code }
}

function Convert-U64($Value, [string]$Code) {
  $parsed = 0L
  if ([string]::IsNullOrEmpty([string]$Value) -or
      -not [UInt64]::TryParse([string]$Value, [ref]$parsed)) {
    throw $Code
  }
  return [UInt64]$parsed
}

function Get-NearestRankP95([UInt64[]]$Values) {
  Assert-True ($Values.Count -gt 0) 'empty_p95_window'
  $sorted = @($Values | Sort-Object)
  $index = [int][Math]::Floor(($sorted.Count * 95 + 99) / 100) - 1
  return [UInt64]$sorted[$index]
}

function Get-Median([UInt64[]]$Values) {
  Assert-True ($Values.Count -gt 0) 'empty_median'
  $sorted = @($Values | Sort-Object)
  $middle = [int][Math]::Floor($sorted.Count / 2)
  if (($sorted.Count % 2) -eq 1) { return [UInt64]$sorted[$middle] }
  return [UInt64][Math]::Floor(([decimal]$sorted[$middle - 1] + [decimal]$sorted[$middle]) / 2)
}

function Get-WorstEventWindow($Rows, [string]$Field, [UInt64]$OriginUs, [ValidateSet('p95','sum','count')][string]$Mode) {
  $bestValue = $null
  $bestStart = $null
  foreach ($start in 0..25) {
    $lower = $OriginUs + [UInt64]($start * 1000000)
    $upper = $OriginUs + [UInt64](($start + 5) * 1000000)
    $windowRows = @($Rows | Where-Object {
      $timestamp = Convert-U64 $_.monotonic_us 'invalid_event_timestamp'
      $timestamp -ge $lower -and $timestamp -lt $upper
    })
    $values = @(if ($Mode -eq 'count') {
      @($windowRows | ForEach-Object { [UInt64]1 })
    } else {
      @($windowRows | Where-Object {
        -not [string]::IsNullOrEmpty([string]$_.$Field)
      } | ForEach-Object { Convert-U64 $_.$Field "invalid_$Field" })
    })
    Assert-True ($values.Count -gt 0) "incomplete_${Field}_window_$start"
    $value = if ($Mode -eq 'p95') {
      Get-NearestRankP95 ([UInt64[]]$values)
    } else {
      [UInt64](($values | Measure-Object -Sum).Sum)
    }
    if ($null -eq $bestValue -or $value -gt $bestValue) {
      $bestValue = $value
      $bestStart = $start
    }
  }
  return [ordered]@{ start_second = $bestStart; value = [UInt64]$bestValue }
}

function Get-PhaseSamples($Rows, [string]$Phase) {
  $phaseRows = @($Rows | Where-Object { $_.phase -eq $Phase } | Sort-Object { [int]$_.second })
  Assert-True ($phaseRows.Count -eq 31) "missing_${Phase}_samples"
  foreach ($second in 0..30) {
    $matches = @($phaseRows | Where-Object { [int]$_.second -eq $second })
    Assert-True ($matches.Count -eq 1) "missing_${Phase}_S$second"
  }
  return $phaseRows
}

function Get-ProcessStatistics($Rows, [string]$Phase) {
  $samples = Get-PhaseSamples $Rows $Phase
  $worstCpu = $null
  $worstStart = $null
  foreach ($start in 0..25) {
    $first = $samples[$start]
    $last = $samples[$start + 5]
    $firstCpu = Convert-U64 $first.process_cpu_total_us 'invalid_process_cpu_total'
    $lastCpu = Convert-U64 $last.process_cpu_total_us 'invalid_process_cpu_total'
    Assert-True ($lastCpu -ge $firstCpu) 'non_monotonic_process_cpu'
    $delta = $lastCpu - $firstCpu
    if ($null -eq $worstCpu -or $delta -gt $worstCpu) {
      $worstCpu = $delta
      $worstStart = $start
    }
  }
  $workingSets = @($samples | ForEach-Object {
    Convert-U64 $_.working_set_bytes 'invalid_working_set'
  })
  $firstMedian = Get-Median ([UInt64[]]@($workingSets[1..5]))
  $lastMedian = Get-Median ([UInt64[]]@($workingSets[26..30]))
  return [ordered]@{
    origin_us = Convert-U64 $samples[0].monotonic_us 'invalid_process_timestamp'
    cpu_worst_window_start_second = $worstStart
    cpu_worst_window_delta_us = [UInt64]$worstCpu
    working_set_max_bytes = [UInt64](($workingSets | Measure-Object -Maximum).Maximum)
    working_set_first_median_bytes = [UInt64]$firstMedian
    working_set_last_median_bytes = [UInt64]$lastMedian
  }
}

function Assert-HeadersAndRows([string]$Path, [string]$ExpectedHeader, [string]$Kind) {
  Assert-True (-not [string]::IsNullOrWhiteSpace($Path)) "missing_${Kind}_path"
  Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) "missing_${Kind}_file"
  $first = Get-Content -LiteralPath $Path -TotalCount 1
  Assert-True ($first -eq $ExpectedHeader) "invalid_${Kind}_schema"
  $rows = @(Import-Csv -LiteralPath $Path)
  Assert-True ($rows.Count -le 16384) "${Kind}_capacity_exceeded"
  Assert-True ($rows.Count -gt 0) "empty_${Kind}_file"
  return $rows
}

function Assert-FileRowIdentity(
  $Rows,
  [string]$ExpectedImplementation,
  [string]$Kind,
  [string[]]$AllowedPhases
) {
  foreach ($row in $Rows) {
    Assert-True ([string]$row.schema_version -eq '1') "invalid_${Kind}_schema_version"
    Assert-True ([string]$row.run_id -match '^[A-Za-z0-9_-]{1,64}$') "invalid_${Kind}_run_id"
    Assert-True ([string]$row.implementation -ceq $ExpectedImplementation) "invalid_${Kind}_implementation"
    Assert-True ($AllowedPhases -ccontains [string]$row.phase) "invalid_${Kind}_phase"
  }
  $runIds = @($Rows | Select-Object -ExpandProperty run_id -Unique)
  Assert-True ($runIds.Count -eq 1) "invalid_${Kind}_run_id"
  return [string]$runIds[0]
}

function Assert-RunRows(
  $Events,
  $ProcessRows,
  [ValidateSet('serial','candidate')][string]$ExpectedImplementation,
  [string]$Kind
) {
  $allPhases = @('VisibleWarmup', 'VisibleMeasurement', 'MinimizedWarmup', 'MinimizedMeasurement', 'Restore')
  $eventRunId = Assert-FileRowIdentity $Events $ExpectedImplementation "${Kind}_events" $allPhases
  $processRunId = Assert-FileRowIdentity $ProcessRows $ExpectedImplementation "${Kind}_process" $MeasuredPhases
  Assert-True ($eventRunId -ceq $processRunId) "${Kind}_run_identity_mismatch"

  $eventPhases = @($Events | Select-Object -ExpandProperty phase -Unique)
  Assert-True ($eventPhases.Count -eq $allPhases.Count) "invalid_${Kind}_events_phase_set"
  foreach ($phase in $allPhases) {
    Assert-True ($eventPhases -ccontains $phase) "invalid_${Kind}_events_phase_set"
  }
  $processPhases = @($ProcessRows | Select-Object -ExpandProperty phase -Unique)
  Assert-True ($processPhases.Count -eq $MeasuredPhases.Count) "invalid_${Kind}_process_phase_set"
  foreach ($phase in $MeasuredPhases) {
    Assert-True ($processPhases -ccontains $phase) "invalid_${Kind}_process_phase_set"
  }

  $boundaries = @($Events | Where-Object { $_.event -ceq 'PhaseBoundary' })
  Assert-True ($boundaries.Count -eq $allPhases.Count) "invalid_${Kind}_events_phase_boundaries"
  for ($index = 0; $index -lt $allPhases.Count; $index++) {
    $phase = $allPhases[$index]
    Assert-True ([string]$boundaries[$index].phase -ceq $phase) "invalid_${Kind}_events_phase_boundaries"
    Assert-True (@($boundaries | Where-Object { $_.phase -ceq $phase }).Count -eq 1) "invalid_${Kind}_events_phase_boundaries"
    $boundaryTimestamp = Convert-U64 $boundaries[$index].monotonic_us 'invalid_event_timestamp'
    Assert-True (@($Events | Where-Object {
      $_.phase -ceq $phase -and (Convert-U64 $_.monotonic_us 'invalid_event_timestamp') -lt $boundaryTimestamp
    }).Count -eq 0) "invalid_${Kind}_events_phase_boundaries"
  }
  return $eventRunId
}

function Assert-MonotonicEvents($Rows) {
  [UInt64]$previous = 0
  $first = $true
  $lastRevision = @{}
  $lastGeneration = @{}
  foreach ($row in $Rows) {
    $timestamp = Convert-U64 $row.monotonic_us 'invalid_event_timestamp'
    if (-not $first) { Assert-True ($timestamp -ge $previous) 'non_monotonic_event_timestamp' }
    $first = $false
    $previous = $timestamp
    if (-not [string]::IsNullOrEmpty($row.session_id) -and
        -not [string]::IsNullOrEmpty($row.generation)) {
      $session = Convert-U64 $row.session_id 'invalid_session_id'
      $generation = Convert-U64 $row.generation 'invalid_generation'
      $sessionKey = [string]$session
      if ($lastGeneration.ContainsKey($sessionKey)) {
        Assert-True ($generation -ge [UInt64]$lastGeneration[$sessionKey]) 'non_monotonic_identity_generation'
      }
      $lastGeneration[$sessionKey] = $generation
    }
    if (-not [string]::IsNullOrEmpty($row.session_id) -and
        -not [string]::IsNullOrEmpty($row.generation) -and
        -not [string]::IsNullOrEmpty($row.revision)) {
      $session = Convert-U64 $row.session_id 'invalid_session_id'
      $generation = Convert-U64 $row.generation 'invalid_generation'
      $revision = Convert-U64 $row.revision 'invalid_revision'
      $key = "$session/$generation"
      if ($lastRevision.ContainsKey($key)) {
        Assert-True ($revision -ge [UInt64]$lastRevision[$key]) 'non_monotonic_identity_revision'
      }
      $lastRevision[$key] = $revision
    }
  }
}

function Assert-MonotonicProcess($Rows) {
  foreach ($phase in $MeasuredPhases) {
    $samples = Get-PhaseSamples $Rows $phase
    [UInt64]$previousTimestamp = 0
    [UInt64]$previousCpu = 0
    foreach ($sample in $samples) {
      $timestamp = Convert-U64 $sample.monotonic_us 'invalid_process_timestamp'
      $cpu = Convert-U64 $sample.process_cpu_total_us 'invalid_process_cpu_total'
      Assert-True ($timestamp -ge $previousTimestamp) 'non_monotonic_process_timestamp'
      Assert-True ($cpu -ge $previousCpu) 'non_monotonic_process_cpu'
      $previousTimestamp = $timestamp
      $previousCpu = $cpu
    }
  }
}

function Get-RunStatistics($Events, $ProcessRows, [string]$Implementation) {
  Assert-MonotonicEvents $Events
  Assert-MonotonicProcess $ProcessRows
  Assert-True (@($Events | Where-Object { $_.event -eq 'StableFault' }).Count -eq 0) 'stable_fault_present'
  $batchEvent = if ($Implementation -eq 'serial') { 'SerialDrain' } else { 'CandidateBatch' }
  $batchRows = @($Events | Where-Object { $_.event -eq $batchEvent })
  Assert-True ($batchRows.Count -gt 0) "missing_${Implementation}_batches"
  Assert-True (@($batchRows | Where-Object { $_.batch_result -ne 'Success' }).Count -eq 0) 'non_success_performance_batch'

  $result = [ordered]@{}
  foreach ($phase in $MeasuredPhases) {
    $process = Get-ProcessStatistics $ProcessRows $phase
    $phaseEvents = @($Events | Where-Object { $_.phase -eq $phase })
    $phaseBatches = @($phaseEvents | Where-Object { $_.event -eq $batchEvent })
    Assert-True ($phaseBatches.Count -gt 0) "missing_${phase}_batches"
    $result[$phase] = [ordered]@{
      batch_cpu_p95 = Get-WorstEventWindow $phaseBatches 'batch_cpu_us' $process.origin_us 'p95'
      mailbox_age_p95 = Get-WorstEventWindow $phaseBatches 'mailbox_age_us' $process.origin_us 'p95'
      scope_begins_sum = Get-WorstEventWindow $phaseBatches 'scope_begins' $process.origin_us 'sum'
      scope_finishes_sum = Get-WorstEventWindow $phaseBatches 'scope_finishes' $process.origin_us 'sum'
      scope_polls_sum = Get-WorstEventWindow $phaseBatches 'scope_polls' $process.origin_us 'sum'
      presentation_sum = Get-WorstEventWindow @($phaseEvents | Where-Object { $_.event -eq 'Presentation' }) '' $process.origin_us 'count'
      input_to_next_present_p95 = Get-WorstEventWindow @($phaseEvents | Where-Object { $_.event -eq 'InputToNextPresent' }) 'input_to_next_present_us' $process.origin_us 'p95'
      frame_response_p95 = Get-WorstEventWindow @($phaseEvents | Where-Object { $_.event -eq 'FrameResponse' }) 'frame_response_ms' $process.origin_us 'p95'
      process = $process
    }
  }
  $result['batch_count'] = $batchRows.Count
  $result['scope_begins_total'] = [UInt64](($batchRows | ForEach-Object { Convert-U64 $_.scope_begins 'invalid_scope_begins' } | Measure-Object -Sum).Sum)
  $result['scope_finishes_total'] = [UInt64](($batchRows | ForEach-Object { Convert-U64 $_.scope_finishes 'invalid_scope_finishes' } | Measure-Object -Sum).Sum)
  $result['scope_polls_total'] = [UInt64](($batchRows | ForEach-Object { Convert-U64 $_.scope_polls 'invalid_scope_polls' } | Measure-Object -Sum).Sum)
  return $result
}

function Assert-FailsWith([scriptblock]$Action, [string]$ExpectedCode) {
  try {
    & $Action
    throw 'selftest_expected_failure_missing'
  } catch {
    Assert-True ($_.Exception.Message -eq $ExpectedCode) "selftest_wrong_failure_$ExpectedCode"
  }
}

function New-SelfTestEventRows([string]$RunId, [string]$Implementation) {
  $rows = @()
  [UInt64]$timestamp = 0
  foreach ($phase in @('VisibleWarmup', 'VisibleMeasurement', 'MinimizedWarmup', 'MinimizedMeasurement', 'Restore')) {
    $rows += [pscustomobject]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = $phase; event = 'PhaseBoundary'; monotonic_us = [string]$timestamp
    }
    $timestamp += 1000000
  }
  return $rows
}

function New-SelfTestProcessRows([string]$RunId, [string]$Implementation) {
  $rows = @()
  foreach ($phase in $MeasuredPhases) {
    foreach ($second in 0..30) {
      $rows += [pscustomobject]@{
        schema_version = '1'; run_id = $RunId; implementation = $Implementation
        phase = $phase; second = [string]$second; monotonic_us = [string]($second * 1000000)
      }
    }
  }
  return $rows
}

function Invoke-SelfTest {
  Assert-True ((Get-NearestRankP95 ([UInt64[]](1..20))) -eq 19) 'selftest_nearest_rank_p95'
  Assert-True ((Get-Median ([UInt64[]]@(9, 1, 5, 3, 7))) -eq 5) 'selftest_odd_median'
  $rows = @()
  foreach ($second in 0..29) {
    $value = if ($second -in 7, 17) { 100 } else { 1 }
    $rows += [pscustomobject]@{ monotonic_us = [string]($second * 1000000); sample = [string]$value }
  }
  $worstP95 = Get-WorstEventWindow $rows 'sample' 0 'p95'
  Assert-True ($worstP95.value -eq 100 -and $worstP95.start_second -eq 3) 'selftest_worst_p95_earliest_tie'
  $sumRows = @()
  foreach ($second in 0..29) {
    $sumRows += [pscustomobject]@{ monotonic_us = [string]($second * 1000000); sample = '2' }
  }
  $worstSum = Get-WorstEventWindow $sumRows 'sample' 0 'sum'
  Assert-True ($worstSum.value -eq 10 -and $worstSum.start_second -eq 0) 'selftest_worst_sum_earliest_tie'
  $singleValueRows = @()
  foreach ($second in 0, 5, 10, 15, 20, 25) {
    $value = if ($second -eq 10) { 90 } else { 1 }
    $singleValueRows += [pscustomobject]@{
      monotonic_us = [string]($second * 1000000); sample = [string]$value
    }
  }
  $singleValueP95 = Get-WorstEventWindow $singleValueRows 'sample' 0 'p95'
  Assert-True ($singleValueP95.value -eq 90 -and $singleValueP95.start_second -eq 6) 'selftest_single_value_window_p95'
  $singleValueCount = Get-WorstEventWindow $singleValueRows '' 0 'count'
  Assert-True ($singleValueCount.value -eq 1 -and $singleValueCount.start_second -eq 0) 'selftest_single_value_window_count'
  $samples = @()
  foreach ($second in 0..30) {
    $samples += [pscustomobject]@{
      phase = 'VisibleMeasurement'; second = [string]$second
      monotonic_us = [string]($second * 1000000)
      process_cpu_total_us = [string]($second * 100)
      working_set_bytes = [string](1000 + $second)
    }
  }
  $statistics = Get-ProcessStatistics $samples 'VisibleMeasurement'
  Assert-True ($statistics.cpu_worst_window_delta_us -eq 500 -and $statistics.cpu_worst_window_start_second -eq 0) 'selftest_cpu_endpoint'
  Assert-True ($statistics.working_set_max_bytes -eq 1030) 'selftest_working_set_maximum'
  Assert-True ($statistics.working_set_first_median_bytes -eq 1003) 'selftest_first_median'
  Assert-True ($statistics.working_set_last_median_bytes -eq 1028) 'selftest_last_median'

  Assert-FailsWith {
    Assert-RunRows (New-SelfTestEventRows 'serial-a' 'serial') (New-SelfTestProcessRows 'serial-b' 'serial') 'serial' 'serial'
  } 'serial_run_identity_mismatch'
  Assert-FailsWith {
    $events = New-SelfTestEventRows 'serial-a' 'serial'
    $events[3].run_id = 'serial-b'
    Assert-RunRows $events (New-SelfTestProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_run_id'
  Assert-FailsWith {
    $events = New-SelfTestEventRows 'serial-a' 'serial'
    $events[2].implementation = 'candidate'
    Assert-RunRows $events (New-SelfTestProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_implementation'
  Assert-FailsWith {
    $processRows = New-SelfTestProcessRows 'serial-a' 'serial'
    $processRows[17].schema_version = '2'
    Assert-RunRows (New-SelfTestEventRows 'serial-a' 'serial') $processRows 'serial' 'serial'
  } 'invalid_serial_process_schema_version'
  Assert-FailsWith {
    $processRows = New-SelfTestProcessRows 'serial-a' 'serial'
    $processRows[0].phase = 'Restore'
    Assert-RunRows (New-SelfTestEventRows 'serial-a' 'serial') $processRows 'serial' 'serial'
  } 'invalid_serial_process_phase'
  Assert-FailsWith {
    $events = @(New-SelfTestEventRows 'serial-a' 'serial')
    $events += [pscustomobject]@{
      schema_version = '1'; run_id = 'serial-a'; implementation = 'serial'
      phase = 'Restore'; event = 'PhaseBoundary'; monotonic_us = '5000000'
    }
    Assert-RunRows $events (New-SelfTestProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_phase_boundaries'
  Assert-FailsWith {
    $events = @(New-SelfTestEventRows 'serial-a' 'serial')
    $swapped = $events[1]
    $events[1] = $events[2]
    $events[2] = $swapped
    Assert-RunRows $events (New-SelfTestProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_phase_boundaries'
  Assert-FailsWith {
    $events = @(New-SelfTestEventRows 'serial-a' 'serial' | Where-Object { $_.phase -ne 'Restore' })
    Assert-RunRows $events (New-SelfTestProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_phase_set'
  Write-Output 'SELFTEST PASS'
}

if ($SelfTest) {
  Invoke-SelfTest
  exit 0
}

foreach ($required in @($SerialEvents, $SerialProcessSamples, $CandidateEvents, $CandidateProcessSamples, $OutputPath)) {
  Assert-True (-not [string]::IsNullOrWhiteSpace($required)) 'missing_required_argument'
}

$serialEventRows = Assert-HeadersAndRows $SerialEvents $EventHeader 'serial_events'
$serialProcessRows = Assert-HeadersAndRows $SerialProcessSamples $ProcessHeader 'serial_process'
$candidateEventRows = Assert-HeadersAndRows $CandidateEvents $EventHeader 'candidate_events'
$candidateProcessRows = Assert-HeadersAndRows $CandidateProcessSamples $ProcessHeader 'candidate_process'
[void](Assert-RunRows $serialEventRows $serialProcessRows 'serial' 'serial')
[void](Assert-RunRows $candidateEventRows $candidateProcessRows 'candidate' 'candidate')
$serial = Get-RunStatistics $serialEventRows $serialProcessRows 'serial'
$candidate = Get-RunStatistics $candidateEventRows $candidateProcessRows 'candidate'

$candidateBatches = @($candidateEventRows | Where-Object { $_.event -eq 'CandidateBatch' })
$scopeRowsExact = @($candidateBatches | Where-Object {
  $_.batch_result -ne 'Success' -or $_.scope_begins -ne '1' -or
  $_.scope_finishes -ne '1' -or $_.scope_polls -ne '1'
}).Count -eq 0
$scopeTotalsExact = $candidate.scope_begins_total -eq $candidate.batch_count -and
  $candidate.scope_finishes_total -eq $candidate.batch_count -and
  $candidate.scope_polls_total -eq $candidate.batch_count

$visibleSerial = $serial.VisibleMeasurement
$visibleCandidate = $candidate.VisibleMeasurement
$latencyGate = $visibleCandidate.batch_cpu_p95.value -le 8000 -and
  ([decimal]$visibleCandidate.batch_cpu_p95.value * 2) -le [decimal]$visibleSerial.batch_cpu_p95.value
$phaseGates = [ordered]@{}
foreach ($phase in $MeasuredPhases) {
  $serialPhase = $serial[$phase]
  $candidatePhase = $candidate[$phase]
  $cpuLimit = [Math]::Max([decimal]$serialPhase.process.cpu_worst_window_delta_us * 1.10,
    [decimal]$serialPhase.process.cpu_worst_window_delta_us + 500000)
  $phaseGates[$phase] = [ordered]@{
    cpu = ([decimal]$candidatePhase.process.cpu_worst_window_delta_us -le $cpuLimit)
    working_set_max = ([decimal]$candidatePhase.process.working_set_max_bytes -le
      [decimal]$serialPhase.process.working_set_max_bytes + 67108864)
    working_set_trend = ([decimal]$candidatePhase.process.working_set_last_median_bytes -le
      [decimal]$candidatePhase.process.working_set_first_median_bytes + 16777216)
    input_to_next_present = (([decimal]$candidatePhase.input_to_next_present_p95.value * 2) -le
      [decimal]$serialPhase.input_to_next_present_p95.value)
    frame_response = ([decimal]$candidatePhase.frame_response_p95.value -le
      [decimal]$serialPhase.frame_response_p95.value)
  }
}
$restoreReceipt = @($candidateEventRows | Where-Object {
  $_.phase -eq 'Restore' -and $_.event -eq 'Presentation' -and
  -not [string]::IsNullOrEmpty($_.session_id) -and
  -not [string]::IsNullOrEmpty($_.generation) -and
  -not [string]::IsNullOrEmpty($_.revision)
}).Count -gt 0

$report = [ordered]@{
  schema_version = 1
  serial = $serial
  candidate = $candidate
  predicates = [ordered]@{
    candidate_batches_success_scope_exact = $scopeRowsExact
    candidate_scope_totals_equal_batch_count = $scopeTotalsExact
    visible_batch_latency_8ms_and_50_percent = $latencyGate
    phase = $phaseGates
    restore_receipt_present = $restoreReceipt
    restore_exact_color_and_working_input_requires_manual_evidence = $true
    fatal_no_present_requires_deterministic_fault_evidence = $true
  }
}

$mandatory = @($scopeRowsExact, $scopeTotalsExact, $latencyGate, $restoreReceipt)
foreach ($phase in $MeasuredPhases) {
  $mandatory += @($phaseGates[$phase].cpu, $phaseGates[$phase].working_set_max,
    $phaseGates[$phase].working_set_trend, $phaseGates[$phase].input_to_next_present,
    $phaseGates[$phase].frame_response)
}
Assert-True (@($mandatory | Where-Object { -not $_ }).Count -eq 0) 'mandatory_performance_predicate_failed'

$outputFullPath = [IO.Path]::GetFullPath($OutputPath)
Assert-True (-not (Test-Path -LiteralPath $outputFullPath)) 'comparison_output_exists'
$parent = Split-Path -Parent $outputFullPath
Assert-True (Test-Path -LiteralPath $parent -PathType Container) 'comparison_output_parent_missing'
$stream = [IO.File]::Open($outputFullPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
try {
  $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
  try { $writer.WriteLine(($report | ConvertTo-Json -Depth 12)) } finally { $writer.Dispose() }
} finally {
  if ($null -ne $stream) { $stream.Dispose() }
}
Write-Output $outputFullPath
