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
$EventRowCapacity = 32768
$ProcessSampleRowLimit = 62
$ProcessSamplePeriodUs = [UInt64]1000000
$ProcessSampleJitterToleranceUs = [UInt64]100000
$EventFields = $EventHeader.Split(',')

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

function Get-CheckedU64Total([UInt64[]]$Values, [string]$Code) {
  $total = [System.Numerics.BigInteger]::Zero
  $maximum = [System.Numerics.BigInteger][UInt64]::MaxValue
  foreach ($value in $Values) {
    $total += [System.Numerics.BigInteger]$value
    Assert-True ($total -le $maximum) "${Code}_overflow"
  }
  return [UInt64]$total
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
      Get-CheckedU64Total ([UInt64[]]$values) "invalid_${Field}_sum"
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

function Get-FieldTotal($Rows, [string]$Field, [string]$Code) {
  if ($Rows.Count -eq 0) { return [UInt64]0 }
  $values = @($Rows | ForEach-Object { Convert-U64 $_.$Field $Code })
  return Get-CheckedU64Total ([UInt64[]]$values) $Code
}

function Get-BatchAndFrameActivity($BatchRows, $FrameResponseRows) {
  return [ordered]@{
    batch_activity_count = [UInt64]$BatchRows.Count
    batch_source_updates_total = Get-FieldTotal $BatchRows 'source_updates' 'invalid_source_updates'
    batch_cpu_total_us = Get-FieldTotal $BatchRows 'batch_cpu_us' 'invalid_batch_cpu_us'
    batch_scope_begins_total = Get-FieldTotal $BatchRows 'scope_begins' 'invalid_scope_begins'
    batch_scope_finishes_total = Get-FieldTotal $BatchRows 'scope_finishes' 'invalid_scope_finishes'
    batch_scope_polls_total = Get-FieldTotal $BatchRows 'scope_polls' 'invalid_scope_polls'
    frame_response_activity_count = [UInt64]$FrameResponseRows.Count
    frame_response_total_ms = Get-FieldTotal $FrameResponseRows 'frame_response_ms' 'invalid_frame_response_ms'
  }
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

function Assert-HeadersAndRows(
  [string]$Path,
  [string]$ExpectedHeader,
  [string]$Kind,
  [Nullable[UInt32]]$MaximumRows
) {
  Assert-True (-not [string]::IsNullOrWhiteSpace($Path)) "missing_${Kind}_path"
  Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) "missing_${Kind}_file"
  $first = Get-Content -LiteralPath $Path -TotalCount 1
  Assert-True ($first -ceq $ExpectedHeader) "invalid_${Kind}_schema"
  $rows = @(Import-Csv -LiteralPath $Path)
  if ($null -eq $MaximumRows) {
    Assert-True ($Kind -like '*_process') "missing_${Kind}_row_limit"
    $MaximumRows = $ProcessSampleRowLimit
  }
  Assert-True ($rows.Count -le $MaximumRows) "${Kind}_capacity_exceeded"
  Assert-True ($rows.Count -gt 0) "empty_${Kind}_file"
  return $rows
}

function Assert-FileRowIdentity(
  $Rows,
  [string]$ExpectedImplementation,
  [string]$Kind,
  [string[]]$AllowedPhases
) {
  $runId = $null
  foreach ($row in $Rows) {
    Assert-True ([string]$row.schema_version -eq '1') "invalid_${Kind}_schema_version"
    Assert-True ([regex]::IsMatch(
      [string]$row.run_id,
      '\A[A-Za-z0-9_-]{1,64}\z',
      [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )) "invalid_${Kind}_run_id"
    Assert-True ([string]$row.implementation -ceq $ExpectedImplementation) "invalid_${Kind}_implementation"
    Assert-True ($AllowedPhases -ccontains [string]$row.phase) "invalid_${Kind}_phase"
    if ($null -eq $runId) {
      $runId = [string]$row.run_id
    } else {
      Assert-True ([string]::Equals($runId, [string]$row.run_id, [StringComparison]::Ordinal)) `
        "invalid_${Kind}_run_id"
    }
  }
  return $runId
}

function Get-OptionalRowField($Row, [string]$Name) {
  $property = $Row.PSObject.Properties[$Name]
  if ($null -eq $property) { return '' }
  return [string]$property.Value
}

function Assert-EventIdentityRows($Rows, [string]$Kind) {
  $activeSession = $null
  $activeGeneration = $null
  $restoreSession = $null
  $restoreGeneration = $null
  $restoreBoundarySeen = $false
  $restorePresentationCount = 0
  foreach ($row in $Rows) {
    $event = [string]$row.event
    $sessionText = Get-OptionalRowField $row 'session_id'
    $generationText = Get-OptionalRowField $row 'generation'
    $revisionText = Get-OptionalRowField $row 'revision'
    $hasSession = -not [string]::IsNullOrEmpty($sessionText)
    $hasGeneration = -not [string]::IsNullOrEmpty($generationText)
    $hasRevision = -not [string]::IsNullOrEmpty($revisionText)
    $hasAnyIdentity = $hasSession -or $hasGeneration -or $hasRevision

    if ($event -ceq 'PhaseBoundary') {
      Assert-True (-not $hasAnyIdentity) "invalid_${Kind}_control_identity"
      if ([string]$row.phase -ceq 'Restore') {
        $restoreBoundarySeen = $true
        $restoreSession = $activeSession
        $restoreGeneration = $activeGeneration
      }
      continue
    }

    if ([string]$row.phase -ceq 'Restore') {
      Assert-True $restoreBoundarySeen "invalid_${Kind}_restore_event_before_boundary"
    }

    $knownIdentityEvent = @(
      'SerialDrain', 'CandidateBatch', 'Presentation',
      'FrameResponse', 'InputToNextPresent', 'StableFault'
    ) -ccontains $event
    Assert-True $knownIdentityEvent "invalid_${Kind}_event"

    if ($event -ceq 'StableFault') {
      Assert-True ((-not $hasAnyIdentity) -or
        ($hasSession -and $hasGeneration -and $hasRevision)) "invalid_${Kind}_event_identity_shape"
      if (-not $hasAnyIdentity) { continue }
    } elseif ($event -ceq 'Presentation') {
      Assert-True ($hasSession -and $hasGeneration -and $hasRevision) `
        "invalid_${Kind}_presentation_identity_shape"
    } elseif ($event -ceq 'CandidateBatch') {
      Assert-True ($hasSession -and $hasGeneration) `
        "invalid_${Kind}_event_identity_shape"
    } else {
      Assert-True ($hasSession -and $hasGeneration -and -not $hasRevision) `
        "invalid_${Kind}_event_identity_shape"
    }

    $session = Convert-U64 $sessionText "invalid_${Kind}_event_session_id"
    $generation = Convert-U64 $generationText "invalid_${Kind}_event_generation"
    Assert-True ($session -gt 0) "invalid_${Kind}_event_session_id_zero"
    Assert-True ($generation -gt 0) "invalid_${Kind}_event_generation_zero"
    if ($hasRevision) {
      $revision = Convert-U64 $revisionText "invalid_${Kind}_event_revision"
      Assert-True ($revision -gt 0) "invalid_${Kind}_event_revision_zero"
    }

    $advancesCurrentGeneration = @('SerialDrain', 'CandidateBatch', 'Presentation') -ccontains $event
    if ($null -ne $activeSession) {
      Assert-True ($session -eq [UInt64]$activeSession) "invalid_${Kind}_event_session_mismatch"
    }
    $isRestorePresentation = [string]$row.phase -ceq 'Restore' -and
      $event -ceq 'Presentation'
    if ($isRestorePresentation) {
      Assert-True ($null -ne $restoreSession -and $null -ne $restoreGeneration -and
        $session -eq [UInt64]$restoreSession -and
        $generation -eq [UInt64]$restoreGeneration) "invalid_${Kind}_restore_presentation_current_identity"
      $restorePresentationCount += 1
    } elseif ([string]$row.phase -ceq 'Restore') {
      Assert-True ($null -ne $restoreSession -and $null -ne $restoreGeneration -and
        $session -eq [UInt64]$restoreSession -and
        $generation -eq [UInt64]$restoreGeneration) "invalid_${Kind}_restore_identity_changed"
    }

    if ($advancesCurrentGeneration) {
      if ($null -ne $activeGeneration) {
        Assert-True ($generation -ge [UInt64]$activeGeneration) `
          "invalid_${Kind}_event_generation_regression"
      }
      $activeSession = $session
      $activeGeneration = $generation
    } else {
      Assert-True ($null -ne $activeSession -and $null -ne $activeGeneration -and
        $session -eq [UInt64]$activeSession -and
        $generation -eq [UInt64]$activeGeneration) "invalid_${Kind}_event_identity_without_current"
    }
  }
  return [pscustomobject]@{
    restore_identity_bearing_presentation_present = [bool]($restorePresentationCount -gt 0)
  }
}

function Assert-ProcessTimelineRows($Rows, $PhaseIntervals, [string]$Kind) {
  foreach ($phase in $MeasuredPhases) {
    $samples = Get-PhaseSamples $Rows $phase
    $interval = $PhaseIntervals[$phase]
    Assert-True ($null -ne $interval) "missing_${Kind}_${phase}_interval"
    $origin = Convert-U64 $samples[0].monotonic_us 'invalid_process_timestamp'
    foreach ($sample in $samples) {
      $timestamp = Convert-U64 $sample.monotonic_us 'invalid_process_timestamp'
      Assert-True ($timestamp -ge [UInt64]$interval.lower -and
        $timestamp -lt [UInt64]$interval.upper) "invalid_${Kind}_process_phase_interval"
      $second = [int]$sample.second
      $expected = [System.Numerics.BigInteger]$origin +
        ([System.Numerics.BigInteger]$second * [System.Numerics.BigInteger]$ProcessSamplePeriodUs)
      Assert-True ($expected -le [System.Numerics.BigInteger][UInt64]::MaxValue) `
        "invalid_${Kind}_process_sample_timeline"
      $difference = [System.Numerics.BigInteger]::Abs(
        [System.Numerics.BigInteger]$timestamp - $expected)
      Assert-True ($difference -le [System.Numerics.BigInteger]$ProcessSampleJitterToleranceUs) `
        "invalid_${Kind}_process_sample_timeline"
    }
    foreach ($start in 0..25) {
      $firstTimestamp = Convert-U64 $samples[$start].monotonic_us 'invalid_process_timestamp'
      $lastTimestamp = Convert-U64 $samples[$start + 5].monotonic_us 'invalid_process_timestamp'
      Assert-True ($lastTimestamp -ge $firstTimestamp) 'non_monotonic_process_timestamp'
      $duration = [System.Numerics.BigInteger]$lastTimestamp -
        [System.Numerics.BigInteger]$firstTimestamp
      $durationDifference = [System.Numerics.BigInteger]::Abs(
        $duration - ([System.Numerics.BigInteger]5 * [System.Numerics.BigInteger]$ProcessSamplePeriodUs))
      Assert-True ($durationDifference -le
        ([System.Numerics.BigInteger]2 * [System.Numerics.BigInteger]$ProcessSampleJitterToleranceUs)) `
        "invalid_${Kind}_process_cpu_window_timeline"
    }
  }
}

function Get-CanonicalEventRowKey($Row) {
  $builder = [Text.StringBuilder]::new()
  foreach ($field in $EventFields) {
    $value = Get-OptionalRowField $Row $field
    [void]$builder.Append($value.Length).Append(':').Append($value).Append(';')
  }
  return $builder.ToString()
}

function Assert-EventFieldShapeRows($Rows, [string]$Kind) {
  $commonFields = @('schema_version', 'run_id', 'implementation', 'phase', 'event', 'monotonic_us')
  $numericFields = @(
    'monotonic_us', 'session_id', 'generation', 'revision', 'source_updates',
    'transactions', 'rectangles', 'batch_cpu_us', 'mailbox_age_us',
    'scope_begins', 'scope_finishes', 'scope_polls', 'process_cpu_total_us',
    'process_cpu_delta_us', 'working_set_bytes', 'frame_response_ms',
    'input_to_next_present_us'
  )
  foreach ($row in $Rows) {
    $event = [string]$row.event
    $allowedFields = @($commonFields)
    $requiredFields = @($commonFields)
    switch -CaseSensitive ($event) {
      'PhaseBoundary' {}
      'Presentation' {
        $allowedFields += @('session_id', 'generation', 'revision')
        $requiredFields += @('session_id', 'generation', 'revision')
      }
      'FrameResponse' {
        $allowedFields += @('session_id', 'generation', 'frame_response_ms')
        $requiredFields += @('session_id', 'generation', 'frame_response_ms')
      }
      'InputToNextPresent' {
        $allowedFields += @('session_id', 'generation', 'input_to_next_present_us')
        $requiredFields += @('session_id', 'generation', 'input_to_next_present_us')
      }
      'SerialDrain' {
        $batchFields = @(
          'batch_result', 'session_id', 'generation', 'source_updates', 'transactions',
          'rectangles', 'batch_cpu_us', 'mailbox_age_us', 'scope_begins',
          'scope_finishes', 'scope_polls'
        )
        $allowedFields += $batchFields
        $requiredFields += $batchFields
      }
      'CandidateBatch' {
        $batchFields = @(
          'batch_result', 'session_id', 'generation', 'source_updates', 'transactions',
          'rectangles', 'batch_cpu_us', 'mailbox_age_us', 'scope_begins',
          'scope_finishes', 'scope_polls'
        )
        $allowedFields += $batchFields + @('revision')
        $requiredFields += $batchFields
      }
      'StableFault' {
        $failureFields = @(
          'batch_result', 'batch_failure_class', 'source_updates', 'transactions',
          'batch_cpu_us', 'mailbox_age_us'
        )
        $allowedFields += $failureFields + @(
          'session_id', 'generation', 'revision', 'scope_begins', 'scope_finishes',
          'scope_polls', 'gpu_fault_code'
        )
        $requiredFields += $failureFields
      }
      default { throw "invalid_${Kind}_event" }
    }

    foreach ($field in $EventFields) {
      $value = Get-OptionalRowField $row $field
      if ($requiredFields -ccontains $field) {
        Assert-True (-not [string]::IsNullOrEmpty($value)) "invalid_${Kind}_event_fields"
      } elseif (-not ($allowedFields -ccontains $field)) {
        Assert-True ([string]::IsNullOrEmpty($value)) "invalid_${Kind}_event_fields"
      }
      if (($numericFields -ccontains $field) -and
          -not [string]::IsNullOrEmpty($value)) {
        $parsed = Convert-U64 $value "invalid_${Kind}_event_fields"
        $canonical = $parsed.ToString([Globalization.CultureInfo]::InvariantCulture)
        Assert-True ($canonical -ceq $value) "invalid_${Kind}_event_fields"
      }
    }

    if ($event -ceq 'SerialDrain' -or $event -ceq 'CandidateBatch') {
      Assert-True ([string]$row.batch_result -ceq 'Success') "invalid_${Kind}_event_fields"
    } elseif ($event -ceq 'StableFault') {
      Assert-True (@('SerialFailure', 'CompileFailure', 'RendererFailure') -ccontains
        [string]$row.batch_result) "invalid_${Kind}_event_fields"
      Assert-True (@('Compiler', 'RendererPlanning', 'RendererExecution', 'Gpu') -ccontains
        [string]$row.batch_failure_class) "invalid_${Kind}_event_fields"
    }
  }
}

function Assert-NoDuplicateEventRows($Rows, [string]$Kind) {
  $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
  foreach ($row in $Rows) {
    $key = Get-CanonicalEventRowKey $row
    Assert-True ($seen.Add($key)) "duplicate_${Kind}_event_row"
  }
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
  $boundaryTimestamps = @()
  for ($index = 0; $index -lt $allPhases.Count; $index++) {
    $phase = $allPhases[$index]
    Assert-True ([string]$boundaries[$index].phase -ceq $phase) "invalid_${Kind}_events_phase_boundaries"
    Assert-True (@($boundaries | Where-Object { $_.phase -ceq $phase }).Count -eq 1) "invalid_${Kind}_events_phase_boundaries"
    $boundaryTimestamp = Convert-U64 $boundaries[$index].monotonic_us 'invalid_event_timestamp'
    $boundaryTimestamps += $boundaryTimestamp
    Assert-True (@($Events | Where-Object {
      $_.phase -ceq $phase -and (Convert-U64 $_.monotonic_us 'invalid_event_timestamp') -lt $boundaryTimestamp
    }).Count -eq 0) "invalid_${Kind}_events_phase_boundaries"
  }
  for ($index = 0; $index -lt $allPhases.Count; $index++) {
    $phase = $allPhases[$index]
    $lower = [UInt64]$boundaryTimestamps[$index]
    $hasUpper = $index + 1 -lt $allPhases.Count
    $upper = if ($hasUpper) { [UInt64]$boundaryTimestamps[$index + 1] } else { [UInt64]::MaxValue }
    Assert-True (@($Events | Where-Object {
      if ([string]$_.phase -cne $phase) { return $false }
      $timestamp = Convert-U64 $_.monotonic_us 'invalid_event_timestamp'
      $timestamp -lt $lower -or ($hasUpper -and $timestamp -ge $upper)
    }).Count -eq 0) "invalid_${Kind}_events_phase_interval"
  }
  $identity = Assert-EventIdentityRows $Events "${Kind}_events"
  Assert-EventFieldShapeRows $Events "${Kind}_events"
  Assert-NoDuplicateEventRows $Events "${Kind}_events"
  $phaseIntervals = @{
    VisibleMeasurement = [pscustomobject]@{
      lower = [UInt64]$boundaryTimestamps[1]
      upper = [UInt64]$boundaryTimestamps[2]
    }
    MinimizedMeasurement = [pscustomobject]@{
      lower = [UInt64]$boundaryTimestamps[3]
      upper = [UInt64]$boundaryTimestamps[4]
    }
  }
  Assert-ProcessTimelineRows $ProcessRows $phaseIntervals $Kind
  Assert-MonotonicEvents $Events
  return [pscustomobject]@{
    run_id = $eventRunId
    restore_identity_bearing_presentation_present =
      [bool]$identity.restore_identity_bearing_presentation_present
  }
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
  Assert-True (@($Events | Where-Object { $_.event -ceq 'StableFault' }).Count -eq 0) 'stable_fault_present'
  $batchEvent = if ($Implementation -eq 'serial') { 'SerialDrain' } else { 'CandidateBatch' }
  $batchRows = @($Events | Where-Object { $_.event -ceq $batchEvent })
  Assert-True ($batchRows.Count -gt 0) "missing_${Implementation}_batches"
  Assert-True (@($batchRows | Where-Object { $_.batch_result -cne 'Success' }).Count -eq 0) 'non_success_performance_batch'

  $result = [ordered]@{}
  foreach ($phase in $MeasuredPhases) {
    $process = Get-ProcessStatistics $ProcessRows $phase
    $phaseEvents = @($Events | Where-Object { $_.phase -ceq $phase })
    $phaseBatches = @($phaseEvents | Where-Object { $_.event -ceq $batchEvent })
    $presentations = @($phaseEvents | Where-Object { $_.event -ceq 'Presentation' })
    $frameResponses = @($phaseEvents | Where-Object { $_.event -ceq 'FrameResponse' })
    $visible = $phase -eq 'VisibleMeasurement'
    if ($visible) {
      Assert-True ($phaseBatches.Count -gt 0) "missing_${phase}_batches"
    }
    $activity = Get-BatchAndFrameActivity $phaseBatches $frameResponses
    $result[$phase] = [ordered]@{
      batch_cpu_p95 = if ($visible) { Get-WorstEventWindow $phaseBatches 'batch_cpu_us' $process.origin_us 'p95' } else { $null }
      mailbox_age_p95 = if ($visible) { Get-WorstEventWindow $phaseBatches 'mailbox_age_us' $process.origin_us 'p95' } else { $null }
      scope_begins_sum = if ($visible) { Get-WorstEventWindow $phaseBatches 'scope_begins' $process.origin_us 'sum' } else { $null }
      scope_finishes_sum = if ($visible) { Get-WorstEventWindow $phaseBatches 'scope_finishes' $process.origin_us 'sum' } else { $null }
      scope_polls_sum = if ($visible) { Get-WorstEventWindow $phaseBatches 'scope_polls' $process.origin_us 'sum' } else { $null }
      presentation_sum = if ($visible) { Get-WorstEventWindow $presentations '' $process.origin_us 'count' } else { $null }
      presentation_activity_count = [UInt64]$presentations.Count
      input_to_next_present_p95 = if ($visible) {
        Get-WorstEventWindow @($phaseEvents | Where-Object { $_.event -ceq 'InputToNextPresent' }) 'input_to_next_present_us' $process.origin_us 'p95'
      } else {
        $null
      }
      frame_response_p95 = if ($visible) { Get-WorstEventWindow $frameResponses 'frame_response_ms' $process.origin_us 'p95' } else { $null }
      batch_activity_count = $activity.batch_activity_count
      batch_source_updates_total = $activity.batch_source_updates_total
      batch_cpu_total_us = $activity.batch_cpu_total_us
      batch_scope_begins_total = $activity.batch_scope_begins_total
      batch_scope_finishes_total = $activity.batch_scope_finishes_total
      batch_scope_polls_total = $activity.batch_scope_polls_total
      frame_response_activity_count = $activity.frame_response_activity_count
      frame_response_total_ms = $activity.frame_response_total_ms
      process = $process
    }
  }
  $result['batch_count'] = $batchRows.Count
  $result['source_updates_total'] = Get-FieldTotal $batchRows 'source_updates' 'invalid_source_updates'
  $result['scope_begins_total'] = Get-FieldTotal $batchRows 'scope_begins' 'invalid_scope_begins'
  $result['scope_finishes_total'] = Get-FieldTotal $batchRows 'scope_finishes' 'invalid_scope_finishes'
  $result['scope_polls_total'] = Get-FieldTotal $batchRows 'scope_polls' 'invalid_scope_polls'
  return $result
}

function Get-VisibleBatchCpu8msAndNoRegression([UInt64]$SerialWorstP95Us, [UInt64]$CandidateWorstP95Us) {
  $noRegressionLimit = [Math]::Max(
    [Math]::Ceiling(([decimal]$SerialWorstP95Us * 110) / 100),
    [decimal]$SerialWorstP95Us + 500)
  return [bool]((([decimal]$CandidateWorstP95Us -le 8000) -and
    ([decimal]$CandidateWorstP95Us -le $noRegressionLimit)))
}

function Get-VisibleScopeAmplificationReduced50Percent($SerialVisible, $CandidateVisible) {
  [UInt64]$serialSourceUpdates = $SerialVisible.batch_source_updates_total
  [UInt64]$candidateSourceUpdates = $CandidateVisible.batch_source_updates_total
  Assert-True ($serialSourceUpdates -gt 0) 'missing_visible_serial_source_updates'
  Assert-True ($candidateSourceUpdates -gt 0) 'missing_visible_candidate_source_updates'
  $left = [System.Numerics.BigInteger]$CandidateVisible.batch_scope_polls_total *
    [System.Numerics.BigInteger]2 * [System.Numerics.BigInteger]$serialSourceUpdates
  $right = [System.Numerics.BigInteger]$SerialVisible.batch_scope_polls_total *
    [System.Numerics.BigInteger]$candidateSourceUpdates
  return [bool]($left -le $right)
}

function Get-MinimizedPresentationPaused($Serial, $Candidate) {
  return [bool](($Serial.MinimizedMeasurement.presentation_activity_count -eq 0) -and
    ($Candidate.MinimizedMeasurement.presentation_activity_count -eq 0))
}

function Get-PhaseGates($Serial, $Candidate) {
  $phaseGates = [ordered]@{}
  foreach ($phase in $MeasuredPhases) {
    $serialPhase = $Serial[$phase]
    $candidatePhase = $Candidate[$phase]
    $cpuLimit = [Math]::Max([decimal]$serialPhase.process.cpu_worst_window_delta_us * 1.10,
      [decimal]$serialPhase.process.cpu_worst_window_delta_us + 500000)
    $inputApplicable = $phase -eq 'VisibleMeasurement'
    $frameResponseApplicable = $phase -eq 'VisibleMeasurement'
    $phaseGates[$phase] = [ordered]@{
      cpu = ([decimal]$candidatePhase.process.cpu_worst_window_delta_us -le $cpuLimit)
      working_set_max = ([decimal]$candidatePhase.process.working_set_max_bytes -le
        [decimal]$serialPhase.process.working_set_max_bytes + 67108864)
      working_set_trend = ([decimal]$candidatePhase.process.working_set_last_median_bytes -le
        [decimal]$candidatePhase.process.working_set_first_median_bytes + 16777216)
      input_to_next_present_applicable = $inputApplicable
      input_to_next_present = if ($inputApplicable) {
        (([decimal]$candidatePhase.input_to_next_present_p95.value * 2) -le
          [decimal]$serialPhase.input_to_next_present_p95.value)
      } else {
        $null
      }
      frame_response_applicable = $frameResponseApplicable
      frame_response = if ($frameResponseApplicable) {
        ([decimal]$candidatePhase.frame_response_p95.value -le
          [decimal]$serialPhase.frame_response_p95.value)
      } else {
        $null
      }
    }
  }
  return $phaseGates
}

function Get-MandatoryPhasePredicates($PhaseGates) {
  $mandatory = @()
  foreach ($phase in $MeasuredPhases) {
    $gate = $PhaseGates[$phase]
    $mandatory += @($gate.cpu, $gate.working_set_max, $gate.working_set_trend)
    if ($gate.input_to_next_present_applicable) {
      $mandatory += @($gate.input_to_next_present)
    }
    if ($gate.frame_response_applicable) {
      $mandatory += @($gate.frame_response)
    }
  }
  return $mandatory
}

function Assert-MandatoryPredicates($Predicates) {
  $failed = @()
  foreach ($entry in $Predicates.GetEnumerator()) {
    if (-not $entry.Value) { $failed += [string]$entry.Key }
  }
  Assert-True ($failed.Count -eq 0) ("mandatory_performance_predicate_failed_{0}" -f ($failed -join ','))
}

function Write-ComparisonOutput([string]$OutputPath, $Report, $MandatoryPredicates) {
  Assert-MandatoryPredicates $MandatoryPredicates
  $outputFullPath = [IO.Path]::GetFullPath($OutputPath)
  Assert-True (-not (Test-Path -LiteralPath $outputFullPath)) 'comparison_output_exists'
  $parent = Split-Path -Parent $outputFullPath
  Assert-True (Test-Path -LiteralPath $parent -PathType Container) 'comparison_output_parent_missing'
  $stream = [IO.File]::Open($outputFullPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
    try { $writer.WriteLine(($Report | ConvertTo-Json -Depth 12)) } finally { $writer.Dispose() }
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
  }
  Write-Output $outputFullPath
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
  foreach ($phaseAndTimestamp in @(
    [pscustomobject]@{ phase = 'VisibleWarmup'; timestamp = [UInt64]0 },
    [pscustomobject]@{ phase = 'VisibleMeasurement'; timestamp = [UInt64]1000000 },
    [pscustomobject]@{ phase = 'MinimizedWarmup'; timestamp = [UInt64]32000000 },
    [pscustomobject]@{ phase = 'MinimizedMeasurement'; timestamp = [UInt64]33000000 },
    [pscustomobject]@{ phase = 'Restore'; timestamp = [UInt64]64000000 }
  )) {
    $rows += [pscustomobject]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = $phaseAndTimestamp.phase; event = 'PhaseBoundary'
      monotonic_us = [string]$phaseAndTimestamp.timestamp
    }
  }
  return $rows
}

function New-SelfTestProcessRows([string]$RunId, [string]$Implementation) {
  $rows = @()
  foreach ($phase in $MeasuredPhases) {
    [UInt64]$origin = if ($phase -ceq 'VisibleMeasurement') { 1000000 } else { 33000000 }
    foreach ($second in 0..30) {
      $rows += [pscustomobject]@{
        schema_version = '1'; run_id = $RunId; implementation = $Implementation
        phase = $phase; second = [string]$second
        monotonic_us = [string]($origin + [UInt64]($second * 1000000))
      }
    }
  }
  return $rows
}

function New-SelfTestCsv([string]$Path, [string]$Header, [int]$RowCount) {
  $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
    try {
      $writer.WriteLine($Header)
      foreach ($index in 0..($RowCount - 1)) { $writer.WriteLine($index) }
    } finally { $writer.Dispose() }
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
  }
}

function New-SelfTestProcessCsv(
  [string]$Path,
  [int]$VisibleCount,
  [int]$MinimizedCount,
  [int]$VisibleDuplicateSecond = -1
) {
  $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
    try {
      $writer.WriteLine($ProcessHeader)
      foreach ($phaseAndCount in @(
        [pscustomobject]@{ phase = 'VisibleMeasurement'; count = $VisibleCount; origin = 0 },
        [pscustomobject]@{ phase = 'MinimizedMeasurement'; count = $MinimizedCount; origin = 40000000 }
      )) {
        foreach ($index in 0..($phaseAndCount.count - 1)) {
          $second = if ($phaseAndCount.phase -eq 'VisibleMeasurement' -and
              $index -eq $VisibleDuplicateSecond) { 30 } else { [Math]::Min($index, 30) }
          $timestamp = $phaseAndCount.origin + ($second * 1000000)
          $writer.WriteLine("1,serial-a,serial,$($phaseAndCount.phase),$second,$timestamp,0,0,1000")
        }
      }
    } finally { $writer.Dispose() }
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
  }
}

function New-SelfTestMetricEventRows(
  [string]$Implementation,
  [UInt64]$VisibleInputValue,
  [bool]$IncludeVisibleInput,
  [bool]$IncludeMinimizedActivity = $true
) {
  $rows = @()
  foreach ($phase in $MeasuredPhases) {
    if ($phase -eq 'MinimizedMeasurement' -and -not $IncludeMinimizedActivity) {
      continue
    }
    [UInt64]$phaseOrigin = if ($phase -eq 'VisibleMeasurement') { 0 } else { 40000000 }
    foreach ($second in 0, 5, 10, 15, 20, 25) {
      $timestamp = [string]($phaseOrigin + [UInt64]($second * 1000000))
      $rows += [pscustomobject]@{
        phase = $phase; event = if ($Implementation -eq 'serial') { 'SerialDrain' } else { 'CandidateBatch' }
        monotonic_us = $timestamp; batch_result = 'Success'; batch_cpu_us = '1'; mailbox_age_us = '1'
        source_updates = '1'; scope_begins = '1'; scope_finishes = '1'; scope_polls = '1'
        session_id = ''; generation = ''; revision = ''
      }
      $rows += [pscustomobject]@{
        phase = $phase; event = 'Presentation'; monotonic_us = $timestamp
        session_id = ''; generation = ''; revision = ''
      }
      $rows += [pscustomobject]@{
        phase = $phase; event = 'FrameResponse'; monotonic_us = $timestamp; frame_response_ms = '1'
        session_id = ''; generation = ''; revision = ''
      }
      if ($phase -eq 'VisibleMeasurement' -and $IncludeVisibleInput) {
        $rows += [pscustomobject]@{
          phase = $phase; event = 'InputToNextPresent'; monotonic_us = $timestamp
          input_to_next_present_us = [string]$VisibleInputValue
          session_id = ''; generation = ''; revision = ''
        }
      }
    }
  }
  return $rows
}

function New-SelfTestMetricProcessRows {
  $rows = @()
  foreach ($phase in $MeasuredPhases) {
    [UInt64]$phaseOrigin = if ($phase -eq 'VisibleMeasurement') { 0 } else { 40000000 }
    foreach ($second in 0..30) {
      $rows += [pscustomobject]@{
        phase = $phase; second = [string]$second; monotonic_us = [string]($phaseOrigin + [UInt64]($second * 1000000))
        process_cpu_total_us = [string]($second * 100); working_set_bytes = [string](1000 + $second)
      }
    }
  }
  return $rows
}

function New-SelfTestComparisonRow([string]$Header, [System.Collections.IDictionary]$Values) {
  $row = [ordered]@{}
  foreach ($field in $Header.Split(',')) { $row[$field] = '' }
  foreach ($entry in $Values.GetEnumerator()) { $row[[string]$entry.Key] = [string]$entry.Value }
  return [pscustomobject]$row
}

function New-SelfTestComparisonEventRows(
  [string]$RunId,
  [ValidateSet('serial','candidate')][string]$Implementation,
  [UInt64]$RestoreSessionId,
  [UInt64]$RestoreGeneration,
  [UInt64]$RestoreRevision
) {
  $rows = @()
  $newBoundary = {
    param([string]$Phase, [UInt64]$Timestamp)
    New-SelfTestComparisonRow $EventHeader ([ordered]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = $Phase; event = 'PhaseBoundary'; monotonic_us = [string]$Timestamp
    })
  }
  $rows += & $newBoundary 'VisibleWarmup' 0
  $rows += & $newBoundary 'VisibleMeasurement' 1000000
  [UInt64]$revision = 1
  foreach ($second in 0, 5, 10, 15, 20, 25) {
    [UInt64]$timestamp = 1000000 + [UInt64]($second * 1000000)
    $batchEvent = if ($Implementation -eq 'serial') { 'SerialDrain' } else { 'CandidateBatch' }
    $batchRevision = if ($Implementation -eq 'candidate') { [string]$revision } else { '' }
    $batchScopes = if ($Implementation -eq 'candidate') { '1' } else { '2' }
    $rows += New-SelfTestComparisonRow $EventHeader ([ordered]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = 'VisibleMeasurement'; event = $batchEvent; batch_result = 'Success'
      monotonic_us = [string]$timestamp; session_id = '1'; generation = '1'; revision = $batchRevision
      source_updates = '1'; transactions = '1'; rectangles = '1'; batch_cpu_us = '1000'
      mailbox_age_us = '1000'; scope_begins = $batchScopes; scope_finishes = $batchScopes
      scope_polls = $batchScopes
    })
    $rows += New-SelfTestComparisonRow $EventHeader ([ordered]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = 'VisibleMeasurement'; event = 'Presentation'; monotonic_us = [string]$timestamp
      session_id = '1'; generation = '1'; revision = [string]$revision
    })
    $rows += New-SelfTestComparisonRow $EventHeader ([ordered]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = 'VisibleMeasurement'; event = 'FrameResponse'; monotonic_us = [string]$timestamp
      session_id = '1'; generation = '1'; frame_response_ms = '1'
    })
    $rows += New-SelfTestComparisonRow $EventHeader ([ordered]@{
      schema_version = '1'; run_id = $RunId; implementation = $Implementation
      phase = 'VisibleMeasurement'; event = 'InputToNextPresent'; monotonic_us = [string]$timestamp
      session_id = '1'; generation = '1'
      input_to_next_present_us = if ($Implementation -eq 'serial') { '40' } else { '20' }
    })
    $revision += 1
  }
  $rows += & $newBoundary 'MinimizedWarmup' 32000000
  $rows += & $newBoundary 'MinimizedMeasurement' 33000000
  $rows += & $newBoundary 'Restore' 64000000
  $rows += New-SelfTestComparisonRow $EventHeader ([ordered]@{
    schema_version = '1'; run_id = $RunId; implementation = $Implementation
    phase = 'Restore'; event = 'Presentation'; monotonic_us = '64000001'
    session_id = [string]$RestoreSessionId; generation = [string]$RestoreGeneration
    revision = [string]$RestoreRevision
  })
  return $rows
}

function New-SelfTestComparisonProcessRows(
  [string]$RunId,
  [ValidateSet('serial','candidate')][string]$Implementation
) {
  $rows = @()
  foreach ($phaseAndOrigin in @(
    [pscustomobject]@{ phase = 'VisibleMeasurement'; origin = [UInt64]1000000 },
    [pscustomobject]@{ phase = 'MinimizedMeasurement'; origin = [UInt64]33000000 }
  )) {
    foreach ($second in 0..30) {
      $rows += New-SelfTestComparisonRow $ProcessHeader ([ordered]@{
        schema_version = '1'; run_id = $RunId; implementation = $Implementation
        phase = $phaseAndOrigin.phase; second = [string]$second
        monotonic_us = [string]($phaseAndOrigin.origin + [UInt64]($second * 1000000))
        process_cpu_total_us = [string]($second * 100); process_cpu_delta_us = '100'
        working_set_bytes = [string](1000 + $second)
      })
    }
  }
  return $rows
}

function Write-SelfTestComparisonCsv([string]$Path, [string]$Header, $Rows) {
  $fields = $Header.Split(',')
  $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
  try {
    $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
    try {
      $writer.WriteLine($Header)
      foreach ($row in $Rows) {
        $values = @($fields | ForEach-Object {
          $value = [string]$row.$_
          if ($value -match '[,\r\n"]') { '"' + $value.Replace('"', '""') + '"' } else { $value }
        })
        $writer.WriteLine(($values -join ','))
      }
    } finally { $writer.Dispose() }
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
  }
}

function Assert-SelfTestProductionForgeryRejected(
  [UInt64]$RestoreSessionId,
  [UInt64]$RestoreGeneration,
  [UInt64]$RestoreRevision,
  [string]$ExpectedCode,
  [string]$FixtureName
) {
  $fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) ("frd-restore-identity-{0}" -f [Guid]::NewGuid())
  [void](New-Item -ItemType Directory -Path $fixtureDirectory)
  try {
    $serialEvents = Join-Path $fixtureDirectory 'serial-events.csv'
    $serialProcess = Join-Path $fixtureDirectory 'serial-process.csv'
    $candidateEvents = Join-Path $fixtureDirectory 'candidate-events.csv'
    $candidateProcess = Join-Path $fixtureDirectory 'candidate-process.csv'
    $output = Join-Path $fixtureDirectory 'comparison.json'
    Write-SelfTestComparisonCsv $serialEvents $EventHeader (New-SelfTestComparisonEventRows 'serial-a' 'serial' $RestoreSessionId $RestoreGeneration $RestoreRevision)
    Write-SelfTestComparisonCsv $serialProcess $ProcessHeader (New-SelfTestComparisonProcessRows 'serial-a' 'serial')
    Write-SelfTestComparisonCsv $candidateEvents $EventHeader (New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7)
    Write-SelfTestComparisonCsv $candidateProcess $ProcessHeader (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate')
    $hostExecutable = if (Test-Path -LiteralPath (Join-Path $PSHOME 'pwsh.exe')) {
      Join-Path $PSHOME 'pwsh.exe'
    } else {
      Join-Path $PSHOME 'powershell.exe'
    }
    $childStdout = Join-Path $fixtureDirectory 'child.stdout.txt'
    $childStderr = Join-Path $fixtureDirectory 'child.stderr.txt'
    $child = Start-Process -FilePath $hostExecutable -ArgumentList @(
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
      '-SerialEvents', $serialEvents, '-SerialProcessSamples', $serialProcess,
      '-CandidateEvents', $candidateEvents, '-CandidateProcessSamples', $candidateProcess,
      '-OutputPath', $output
    ) -Wait -PassThru -RedirectStandardOutput $childStdout -RedirectStandardError $childStderr
    $childOutput = @(
      Get-Content -LiteralPath $childStdout -ErrorAction SilentlyContinue
      Get-Content -LiteralPath $childStderr -ErrorAction SilentlyContinue
    )
    $childExitCode = $child.ExitCode
    Assert-True ($childExitCode -ne 0) "selftest_${FixtureName}_not_rejected"
    Assert-True (-not (Test-Path -LiteralPath $output)) "selftest_${FixtureName}_created_output"
    Assert-True (($childOutput -join "`n") -match [regex]::Escape($ExpectedCode)) "selftest_${FixtureName}_wrong_failure"
  } finally {
    Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }
}

function Assert-SelfTestTopLevelRejected(
  $SerialEventRows,
  $SerialProcessRows,
  $CandidateEventRows,
  $CandidateProcessRows,
  [string]$ExpectedCode,
  [string]$FixtureName
) {
  $fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) ("frd-comparator-rejection-{0}" -f [Guid]::NewGuid())
  [void](New-Item -ItemType Directory -Path $fixtureDirectory)
  try {
    $serialEvents = Join-Path $fixtureDirectory 'serial-events.csv'
    $serialProcess = Join-Path $fixtureDirectory 'serial-process.csv'
    $candidateEvents = Join-Path $fixtureDirectory 'candidate-events.csv'
    $candidateProcess = Join-Path $fixtureDirectory 'candidate-process.csv'
    $output = Join-Path $fixtureDirectory 'comparison.json'
    Write-SelfTestComparisonCsv $serialEvents $EventHeader $SerialEventRows
    Write-SelfTestComparisonCsv $serialProcess $ProcessHeader $SerialProcessRows
    Write-SelfTestComparisonCsv $candidateEvents $EventHeader $CandidateEventRows
    Write-SelfTestComparisonCsv $candidateProcess $ProcessHeader $CandidateProcessRows
    $hostExecutable = if (Test-Path -LiteralPath (Join-Path $PSHOME 'pwsh.exe')) {
      Join-Path $PSHOME 'pwsh.exe'
    } else {
      Join-Path $PSHOME 'powershell.exe'
    }
    $childStdout = Join-Path $fixtureDirectory 'child.stdout.txt'
    $childStderr = Join-Path $fixtureDirectory 'child.stderr.txt'
    $child = Start-Process -FilePath $hostExecutable -ArgumentList @(
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
      '-SerialEvents', $serialEvents, '-SerialProcessSamples', $serialProcess,
      '-CandidateEvents', $candidateEvents, '-CandidateProcessSamples', $candidateProcess,
      '-OutputPath', $output
    ) -Wait -PassThru -RedirectStandardOutput $childStdout -RedirectStandardError $childStderr
    $stdout = @(Get-Content -LiteralPath $childStdout -ErrorAction SilentlyContinue)
    $stderr = @(Get-Content -LiteralPath $childStderr -ErrorAction SilentlyContinue)
    Assert-True ($child.ExitCode -ne 0) "selftest_${FixtureName}_not_rejected"
    Assert-True (-not (Test-Path -LiteralPath $output)) "selftest_${FixtureName}_created_output"
    Assert-True ([string]::IsNullOrWhiteSpace(($stdout -join "`n"))) "selftest_${FixtureName}_stdout_not_empty"
    Assert-True (($stderr -join "`n") -match [regex]::Escape($ExpectedCode)) "selftest_${FixtureName}_wrong_failure"
  } finally {
    Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }
}

function Assert-SelfTestTopLevelAccepted(
  $SerialEventRows,
  $SerialProcessRows,
  $CandidateEventRows,
  $CandidateProcessRows,
  [string]$FixtureName
) {
  $fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) ("frd-comparator-acceptance-{0}" -f [Guid]::NewGuid())
  [void](New-Item -ItemType Directory -Path $fixtureDirectory)
  try {
    $serialEvents = Join-Path $fixtureDirectory 'serial-events.csv'
    $serialProcess = Join-Path $fixtureDirectory 'serial-process.csv'
    $candidateEvents = Join-Path $fixtureDirectory 'candidate-events.csv'
    $candidateProcess = Join-Path $fixtureDirectory 'candidate-process.csv'
    $output = Join-Path $fixtureDirectory 'comparison.json'
    Write-SelfTestComparisonCsv $serialEvents $EventHeader $SerialEventRows
    Write-SelfTestComparisonCsv $serialProcess $ProcessHeader $SerialProcessRows
    Write-SelfTestComparisonCsv $candidateEvents $EventHeader $CandidateEventRows
    Write-SelfTestComparisonCsv $candidateProcess $ProcessHeader $CandidateProcessRows
    $hostExecutable = if (Test-Path -LiteralPath (Join-Path $PSHOME 'pwsh.exe')) {
      Join-Path $PSHOME 'pwsh.exe'
    } else {
      Join-Path $PSHOME 'powershell.exe'
    }
    $childStdout = Join-Path $fixtureDirectory 'child.stdout.txt'
    $childStderr = Join-Path $fixtureDirectory 'child.stderr.txt'
    $child = Start-Process -FilePath $hostExecutable -ArgumentList @(
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
      '-SerialEvents', $serialEvents, '-SerialProcessSamples', $serialProcess,
      '-CandidateEvents', $candidateEvents, '-CandidateProcessSamples', $candidateProcess,
      '-OutputPath', $output
    ) -Wait -PassThru -RedirectStandardOutput $childStdout -RedirectStandardError $childStderr
    $stderr = @(Get-Content -LiteralPath $childStderr -ErrorAction SilentlyContinue)
    Assert-True ($child.ExitCode -eq 0) "selftest_${FixtureName}_rejected"
    Assert-True (Test-Path -LiteralPath $output -PathType Leaf) "selftest_${FixtureName}_missing_output"
    Assert-True ([string]::IsNullOrWhiteSpace(($stderr -join "`n"))) "selftest_${FixtureName}_stderr_not_empty"
  } finally {
    Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }
}

function Invoke-SelfTest {
  $multilineCandidateEvents = New-SelfTestComparisonEventRows "candidate-a`n" 'candidate' 1 1 7
  $multilineCandidateProcess = New-SelfTestComparisonProcessRows "candidate-a`n" 'candidate'
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $multilineCandidateEvents `
    $multilineCandidateProcess `
    'invalid_candidate_events_run_id' 'multiline_candidate_run_id'

  $duplicatedCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $duplicateCandidateBatch = @($duplicatedCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $duplicateCandidateIndex = [Array]::IndexOf($duplicatedCandidateEvents, $duplicateCandidateBatch)
  $duplicatedCandidateEvents = @(
    $duplicatedCandidateEvents[0..$duplicateCandidateIndex]
    $duplicateCandidateBatch
    $duplicatedCandidateEvents[($duplicateCandidateIndex + 1)..($duplicatedCandidateEvents.Count - 1)]
  )
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $duplicatedCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'duplicate_candidate_events_event_row' 'duplicate_candidate_batch'

  $ignoredFieldCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $ignoredFieldSourceBatch = @($ignoredFieldCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $ignoredFieldBatch = New-SelfTestComparisonRow $EventHeader ([ordered]@{})
  foreach ($field in $EventFields) {
    $ignoredFieldBatch.$field = [string]$ignoredFieldSourceBatch.$field
  }
  $ignoredFieldBatch.gpu_fault_code = 'IGNORED'
  $ignoredFieldIndex = [Array]::IndexOf($ignoredFieldCandidateEvents, $ignoredFieldSourceBatch)
  $ignoredFieldCandidateEvents = @(
    $ignoredFieldCandidateEvents[0..$ignoredFieldIndex]
    $ignoredFieldBatch
    $ignoredFieldCandidateEvents[($ignoredFieldIndex + 1)..($ignoredFieldCandidateEvents.Count - 1)]
  )
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $ignoredFieldCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_event_fields' 'ignored_gpu_fault_duplicate_candidate_batch'

  $nonCanonicalCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $nonCanonicalSourceBatch = @($nonCanonicalCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $nonCanonicalBatch = New-SelfTestComparisonRow $EventHeader ([ordered]@{})
  foreach ($field in $EventFields) {
    $nonCanonicalBatch.$field = [string]$nonCanonicalSourceBatch.$field
  }
  $nonCanonicalBatch.source_updates = '01'
  $nonCanonicalIndex = [Array]::IndexOf($nonCanonicalCandidateEvents, $nonCanonicalSourceBatch)
  $nonCanonicalCandidateEvents = @(
    $nonCanonicalCandidateEvents[0..$nonCanonicalIndex]
    $nonCanonicalBatch
    $nonCanonicalCandidateEvents[($nonCanonicalIndex + 1)..($nonCanonicalCandidateEvents.Count - 1)]
  )
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $nonCanonicalCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_event_fields' 'noncanonical_numeric_duplicate_candidate_batch'

  $lowercaseSuccessEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $lowercaseSuccessBatch = @($lowercaseSuccessEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $lowercaseSuccessBatch.batch_result = 'success'
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $lowercaseSuccessEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_event_fields' 'lowercase_candidate_batch_success'

  $installedSurfaceCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $installedSurfaceCandidateBatch = @($installedSurfaceCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $installedSurfaceCandidateBatch.revision = ''
  Assert-SelfTestTopLevelAccepted `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $installedSurfaceCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'candidate_installed_surface_identity'

  $partialCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $partialCandidateBatch = @($partialCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $partialCandidateBatch.generation = ''
  $partialCandidateBatch.revision = ''
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $partialCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_event_identity_shape' 'partial_candidate_batch_identity'

  $caseFoldedCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7
  $caseFoldedCandidateBatch = @($caseFoldedCandidateEvents | Where-Object {
    $_.event -ceq 'CandidateBatch'
  })[0]
  $caseFoldedCandidateBatch.event = 'candidatebatch'
  $caseFoldedCandidateBatch.session_id = ''
  $caseFoldedCandidateBatch.generation = ''
  $caseFoldedCandidateBatch.revision = ''
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $caseFoldedCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_event' 'case_folded_candidate_batch'

  $restoreRebasedCandidateEvents = New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 2 8
  $restoreRebasedPrefix = @(
    $restoreRebasedCandidateEvents[0..($restoreRebasedCandidateEvents.Count - 2)]
  )
  $restoreRebasedPresentation = $restoreRebasedCandidateEvents[-1]
  $restoreRebasedCandidateEvents = @($restoreRebasedPrefix)
  $restoreRebasedCandidateEvents += New-SelfTestComparisonRow $EventHeader ([ordered]@{
    schema_version = '1'; run_id = 'candidate-a'; implementation = 'candidate'
    phase = 'Restore'; event = 'CandidateBatch'; batch_result = 'Success'
    monotonic_us = '64000000'; session_id = '1'; generation = '2'; revision = ''
    source_updates = '1'; transactions = '1'; rectangles = '1'; batch_cpu_us = '1000'
    mailbox_age_us = '1000'; scope_begins = '1'; scope_finishes = '1'; scope_polls = '1'
  })
  $restoreRebasedCandidateEvents += $restoreRebasedPresentation
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    $restoreRebasedCandidateEvents `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_candidate_events_restore_identity_changed' 'restore_candidate_batch_rebase'

  $poisonedBoundaryEvents = New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 2 7
  $poisonedRestoreBoundary = @($poisonedBoundaryEvents | Where-Object {
    $_.phase -ceq 'Restore' -and $_.event -ceq 'PhaseBoundary'
  })[0]
  $poisonedRestoreBoundary.session_id = '1'
  $poisonedRestoreBoundary.generation = '2'
  Assert-SelfTestTopLevelRejected `
    $poisonedBoundaryEvents `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    (New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_serial_events_control_identity' 'restore_boundary_identity_poison'

  $unknownIdentityEvents = New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 2 7
  $unknownIdentityPrefix = @($unknownIdentityEvents[0..($unknownIdentityEvents.Count - 2)])
  $unknownIdentitySuffix = $unknownIdentityEvents[-1]
  $unknownIdentityEvents = @($unknownIdentityPrefix)
  $unknownIdentityEvents += New-SelfTestComparisonRow $EventHeader ([ordered]@{
    schema_version = '1'; run_id = 'serial-a'; implementation = 'serial'
    phase = 'Restore'; event = 'UnknownReceipt'; monotonic_us = '64000000'
    session_id = '1'; generation = '2'
  })
  $unknownIdentityEvents += $unknownIdentitySuffix
  Assert-SelfTestTopLevelRejected `
    $unknownIdentityEvents `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') `
    (New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_serial_events_event' 'unknown_event_identity_poison'

  $fakeTimelineProcess = New-SelfTestComparisonProcessRows 'serial-a' 'serial'
  foreach ($row in $fakeTimelineProcess) {
    [UInt64]$origin = if ($row.phase -ceq 'VisibleMeasurement') { 1000000 } else { 33000000 }
    $row.monotonic_us = [string]($origin + [UInt64]$row.second)
  }
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    $fakeTimelineProcess `
    (New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_serial_process_sample_timeline' 'one_microsecond_process_timeline'

  $outOfPhaseProcess = New-SelfTestComparisonProcessRows 'serial-a' 'serial'
  $outOfPhaseS30 = @($outOfPhaseProcess | Where-Object {
    $_.phase -ceq 'VisibleMeasurement' -and $_.second -ceq '30'
  })[0]
  $outOfPhaseS30.monotonic_us = '32000000'
  Assert-SelfTestTopLevelRejected `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7) `
    $outOfPhaseProcess `
    (New-SelfTestComparisonEventRows 'candidate-a' 'candidate' 1 1 7) `
    (New-SelfTestComparisonProcessRows 'candidate-a' 'candidate') `
    'invalid_serial_process_phase_interval' 'out_of_phase_process_sample'

  Assert-SelfTestProductionForgeryRejected 1 2 7 `
    'invalid_serial_events_restore_presentation_current_identity' 'isolated_restore_generation'
  Assert-SelfTestProductionForgeryRejected 9 1 7 `
    'invalid_serial_events_event_session_mismatch' 'isolated_restore_session'
  Assert-SelfTestProductionForgeryRejected 0 0 0 `
    'invalid_serial_events_event_session_id_zero' 'zero_restore_identity'
  Assert-FailsWith {
    Assert-RunRows (New-SelfTestComparisonEventRows 'serial-a' 'serial' 0 0 0) `
      (New-SelfTestComparisonProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_event_session_id_zero'
  Assert-FailsWith {
    Assert-RunRows (New-SelfTestComparisonEventRows 'serial-a' 'serial' 9 1 7) `
      (New-SelfTestComparisonProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_event_session_mismatch'
  Assert-FailsWith {
    Assert-RunRows (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 2 7) `
      (New-SelfTestComparisonProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_restore_presentation_current_identity'
  Assert-FailsWith {
    $events = New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 7
    $events[-1].session_id = ''
    $events[-1].generation = ''
    $events[-1].revision = ''
    Assert-RunRows $events (New-SelfTestComparisonProcessRows 'serial-a' 'serial') 'serial' 'serial'
  } 'invalid_serial_events_presentation_identity_shape'
  $revisionEvolution = Assert-RunRows `
    (New-SelfTestComparisonEventRows 'serial-a' 'serial' 1 1 9) `
    (New-SelfTestComparisonProcessRows 'serial-a' 'serial') 'serial' 'serial'
  Assert-True $revisionEvolution.restore_identity_bearing_presentation_present `
    'selftest_legitimate_revision_evolution_rejected'
  Assert-True ((Get-NearestRankP95 ([UInt64[]](1..20))) -eq 19) 'selftest_nearest_rank_p95'
  Assert-True ((Get-Median ([UInt64[]]@(9, 1, 5, 3, 7))) -eq 5) 'selftest_odd_median'
  $largeExactRows = @(
    [pscustomobject]@{ value = '9007199254740992' },
    [pscustomobject]@{ value = '1' }
  )
  Assert-True ((Get-FieldTotal $largeExactRows 'value' 'selftest_checked_total') -eq [UInt64]9007199254740993) 'selftest_checked_u64_total'
  $overflowRows = @(
    [pscustomobject]@{ value = '18446744073709551615' },
    [pscustomobject]@{ value = '1' }
  )
  Assert-FailsWith {
    Get-FieldTotal $overflowRows 'value' 'selftest_checked_total'
  } 'selftest_checked_total_overflow'
  $largeWindowRows = @()
  foreach ($second in 0..29) {
    $value = if ($second -eq 0) { '9007199254740993' } else { '1' }
    $largeWindowRows += [pscustomobject]@{ monotonic_us = [string]($second * 1000000); sample = $value }
  }
  $largeWindowSum = Get-WorstEventWindow $largeWindowRows 'sample' 0 'sum'
  Assert-True ($largeWindowSum.value -eq [UInt64]9007199254740997 -and
    $largeWindowSum.start_second -eq 0) 'selftest_checked_u64_window_sum'
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

  $capacityFixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) ("frd-comparator-capacity-{0}" -f [Guid]::NewGuid())
  [void](New-Item -ItemType Directory -Path $capacityFixtureDirectory)
  try {
    $event16385 = Join-Path $capacityFixtureDirectory 'events-16385.csv'
    $event32768 = Join-Path $capacityFixtureDirectory 'events-32768.csv'
    $event32769 = Join-Path $capacityFixtureDirectory 'events-32769.csv'
    $serialOverCapacityProcess = Join-Path $capacityFixtureDirectory 'serial-process-63.csv'
    $candidateOverCapacityProcess = Join-Path $capacityFixtureDirectory 'candidate-process-63.csv'
    $malformedProcess = Join-Path $capacityFixtureDirectory 'process-duplicate-s30.csv'
    New-SelfTestCsv $event16385 $EventHeader 16385
    New-SelfTestCsv $event32768 $EventHeader 32768
    New-SelfTestCsv $event32769 $EventHeader 32769
    New-SelfTestProcessCsv $serialOverCapacityProcess 32 31
    New-SelfTestProcessCsv $candidateOverCapacityProcess 32 31
    New-SelfTestProcessCsv $malformedProcess 31 31 29
    Assert-True (@(Assert-HeadersAndRows $event16385 $EventHeader 'serial_events' $EventRowCapacity).Count -eq 16385) 'selftest_event_16385_rejected'
    Assert-True (@(Assert-HeadersAndRows $event32768 $EventHeader 'serial_events' $EventRowCapacity).Count -eq 32768) 'selftest_event_32768_rejected'
    Assert-FailsWith {
      Assert-HeadersAndRows $event32769 $EventHeader 'serial_events' $EventRowCapacity
    } 'serial_events_capacity_exceeded'
    Assert-FailsWith {
      Assert-HeadersAndRows $serialOverCapacityProcess $ProcessHeader 'serial_process'
    } 'serial_process_capacity_exceeded'
    Assert-FailsWith {
      Assert-HeadersAndRows $candidateOverCapacityProcess $ProcessHeader 'candidate_process'
    } 'candidate_process_capacity_exceeded'
    $malformedProcessRows = @(Assert-HeadersAndRows $malformedProcess $ProcessHeader 'serial_process')
    Assert-FailsWith {
      Get-RunStatistics (New-SelfTestMetricEventRows 'serial' 40 $true) $malformedProcessRows 'serial'
    } 'missing_VisibleMeasurement_S29'
  } finally {
    Remove-Item -LiteralPath $capacityFixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }

  $metricProcessRows = New-SelfTestMetricProcessRows
  $serialMetricStatistics = Get-RunStatistics (New-SelfTestMetricEventRows 'serial' 40 $true) $metricProcessRows 'serial'
  $candidateMetricStatistics = Get-RunStatistics (New-SelfTestMetricEventRows 'candidate' 20 $true) $metricProcessRows 'candidate'
  Assert-True ($null -eq $candidateMetricStatistics.MinimizedMeasurement.input_to_next_present_p95) 'selftest_minimized_input_not_applicable'
  Assert-FailsWith {
    Get-RunStatistics (New-SelfTestMetricEventRows 'serial' 40 $false) $metricProcessRows 'serial'
  } 'incomplete_input_to_next_present_us_window_0'
  $phaseGates = Get-PhaseGates $serialMetricStatistics $candidateMetricStatistics
  Assert-True ($candidateMetricStatistics.VisibleMeasurement.input_to_next_present_p95.value -eq 20 -and
    $phaseGates.VisibleMeasurement.input_to_next_present_applicable -and
    $phaseGates.VisibleMeasurement.input_to_next_present) 'selftest_visible_input_real_and_applicable'
  Assert-True (-not $phaseGates.MinimizedMeasurement.input_to_next_present_applicable -and
    $null -eq $phaseGates.MinimizedMeasurement.input_to_next_present) 'selftest_minimized_input_predicate_not_applicable'
  $phaseMandatory = Get-MandatoryPhasePredicates $phaseGates
  Assert-True ($phaseMandatory.Count -eq 8 -and @($phaseMandatory | Where-Object { -not $_ }).Count -eq 0) 'selftest_mandatory_excludes_minimized_input'
  $phaseGates.VisibleMeasurement.input_to_next_present = $false
  $phaseMandatory = Get-MandatoryPhasePredicates $phaseGates
  Assert-True ($phaseMandatory.Count -eq 8 -and @($phaseMandatory | Where-Object { -not $_ }).Count -eq 1) 'selftest_mandatory_includes_visible_input'

  # Task 7: static minimized windows are valid measurements, but any actual
  # presentation remains forbidden until Restore.
  $task7Failures = @()
  $idleSerialEvents = New-SelfTestMetricEventRows 'serial' 40 $true $false
  $idleCandidateEvents = New-SelfTestMetricEventRows 'candidate' 20 $true $false
  try {
    $idleSerialStatistics = Get-RunStatistics $idleSerialEvents $metricProcessRows 'serial'
    $idleCandidateStatistics = Get-RunStatistics $idleCandidateEvents $metricProcessRows 'candidate'
    $idleMinimized = $idleCandidateStatistics.MinimizedMeasurement
    $idleSerialMinimized = $idleSerialStatistics.MinimizedMeasurement
    $idleMinimizedStatistics = @($idleSerialMinimized, $idleMinimized)
    $idleMinimizedInvalid = @($idleMinimizedStatistics | Where-Object {
      $null -ne $_.batch_cpu_p95 -or
      $null -ne $_.mailbox_age_p95 -or
      $null -ne $_.scope_begins_sum -or
      $null -ne $_.scope_finishes_sum -or
      $null -ne $_.scope_polls_sum -or
      $null -ne $_.presentation_sum -or
      $null -ne $_.frame_response_p95 -or
      $_.presentation_activity_count -ne 0 -or
      $_.batch_activity_count -ne 0 -or
      $_.frame_response_activity_count -ne 0 -or
      $_.batch_source_updates_total -ne 0 -or
      $_.batch_cpu_total_us -ne 0 -or
      $_.batch_scope_begins_total -ne 0 -or
      $_.batch_scope_finishes_total -ne 0 -or
      $_.batch_scope_polls_total -ne 0 -or
      $_.frame_response_total_ms -ne 0
    }).Count -ne 0
    if ($idleMinimizedInvalid -or
        -not (Get-MinimizedPresentationPaused $idleSerialStatistics $idleCandidateStatistics)) {
      $task7Failures += 'idle_minimized_statistics'
    }
  } catch {
    $task7Failures += 'idle_minimized_statistics'
  }
  try {
    $presentingCandidateEvents = @($idleCandidateEvents)
    $presentingCandidateEvents += [pscustomobject]@{
      phase = 'MinimizedMeasurement'; event = 'Presentation'; monotonic_us = '40000000'
      session_id = ''; generation = ''; revision = ''
    }
    $presentingCandidateStatistics = Get-RunStatistics $presentingCandidateEvents $metricProcessRows 'candidate'
    if (Get-MinimizedPresentationPaused $idleSerialStatistics $presentingCandidateStatistics) {
      $task7Failures += 'minimized_presentation_rejection'
    }
  } catch {
    $task7Failures += 'minimized_presentation_rejection'
  }
  try {
    $scopeSerial = [pscustomobject]@{ batch_scope_polls_total = [UInt64]100; batch_source_updates_total = [UInt64]100 }
    $scopeCandidateReduced = [pscustomobject]@{ batch_scope_polls_total = [UInt64]50; batch_source_updates_total = [UInt64]100 }
    $scopeCandidateUnreduced = [pscustomobject]@{ batch_scope_polls_total = [UInt64]51; batch_source_updates_total = [UInt64]100 }
    $scopeReduced = Get-VisibleScopeAmplificationReduced50Percent $scopeSerial $scopeCandidateReduced
    $scopeUnreduced = Get-VisibleScopeAmplificationReduced50Percent $scopeSerial $scopeCandidateUnreduced
    $largeScope = [UInt64]9007199254740993
    $largeScopeSerial = [pscustomobject]@{ batch_scope_polls_total = $largeScope; batch_source_updates_total = $largeScope }
    $largeScopeCandidate = [pscustomobject]@{ batch_scope_polls_total = $largeScope; batch_source_updates_total = [UInt64]18014398509481986 }
    $largeScopeReduced = Get-VisibleScopeAmplificationReduced50Percent $largeScopeSerial $largeScopeCandidate
    if ((Get-VisibleBatchCpu8msAndNoRegression 1000 1500) -ne $true -or
        (Get-VisibleBatchCpu8msAndNoRegression 1000 1501) -ne $false -or
        (Get-VisibleBatchCpu8msAndNoRegression 10000 8000) -ne $true -or
        (Get-VisibleBatchCpu8msAndNoRegression 7001 7702) -ne $true -or
        (Get-VisibleBatchCpu8msAndNoRegression 7001 7703) -ne $false -or
        (Get-VisibleBatchCpu8msAndNoRegression 10000 8001) -ne $false -or
        $scopeReduced -ne $true -or $scopeUnreduced -ne $false -or $largeScopeReduced -ne $true) {
      $task7Failures += 'visible_bounded_predicates'
    }
  } catch {
    $task7Failures += 'visible_bounded_predicates'
  }
  Assert-True ($task7Failures.Count -eq 0) ("selftest_task7_approved_behavior_absent_{0}" -f ($task7Failures -join '_'))
  $zeroSerialSource = [pscustomobject]@{ batch_scope_polls_total = [UInt64]1; batch_source_updates_total = [UInt64]0 }
  $nonzeroCandidateSource = [pscustomobject]@{ batch_scope_polls_total = [UInt64]0; batch_source_updates_total = [UInt64]1 }
  Assert-FailsWith {
    Get-VisibleScopeAmplificationReduced50Percent $zeroSerialSource $nonzeroCandidateSource
  } 'missing_visible_serial_source_updates'
  $nonzeroSerialSource = [pscustomobject]@{ batch_scope_polls_total = [UInt64]1; batch_source_updates_total = [UInt64]1 }
  $zeroCandidateSource = [pscustomobject]@{ batch_scope_polls_total = [UInt64]0; batch_source_updates_total = [UInt64]0 }
  Assert-FailsWith {
    Get-VisibleScopeAmplificationReduced50Percent $nonzeroSerialSource $zeroCandidateSource
  } 'missing_visible_candidate_source_updates'
  $failedPredicateOutput = Join-Path ([IO.Path]::GetTempPath()) ("frd-task7-selftest-{0}.json" -f [Guid]::NewGuid())
  $diagnosticPredicates = [ordered]@{
    candidate_batches_success_scope_exact = $true
    visible_scope_amplification_reduced_50_percent = $false
    minimized_presentation_paused = $false
  }
  Assert-FailsWith {
    Write-ComparisonOutput $failedPredicateOutput ([ordered]@{ schema_version = 1 }) $diagnosticPredicates
  } 'mandatory_performance_predicate_failed_visible_scope_amplification_reduced_50_percent,minimized_presentation_paused'
  Assert-True (-not (Test-Path -LiteralPath $failedPredicateOutput)) 'selftest_mandatory_failure_created_output'

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
    $events = @(New-SelfTestEventRows 'candidate-a' 'candidate')
    $events += [pscustomobject]@{
      schema_version = '1'; run_id = 'candidate-a'; implementation = 'candidate'
      phase = 'VisibleMeasurement'; event = 'CandidateBatch'; batch_result = 'Success'
      monotonic_us = '32000000'; source_updates = '18446744073709551615'
      scope_begins = '1'; scope_finishes = '1'; scope_polls = '1'
    }
    Assert-RunRows $events (New-SelfTestProcessRows 'candidate-a' 'candidate') 'candidate' 'candidate'
  } 'invalid_candidate_events_phase_interval'
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

$serialEventRows = Assert-HeadersAndRows $SerialEvents $EventHeader 'serial_events' $EventRowCapacity
$serialProcessRows = Assert-HeadersAndRows $SerialProcessSamples $ProcessHeader 'serial_process' $ProcessSampleRowLimit
$candidateEventRows = Assert-HeadersAndRows $CandidateEvents $EventHeader 'candidate_events' $EventRowCapacity
$candidateProcessRows = Assert-HeadersAndRows $CandidateProcessSamples $ProcessHeader 'candidate_process' $ProcessSampleRowLimit
$serialRun = Assert-RunRows $serialEventRows $serialProcessRows 'serial' 'serial'
$candidateRun = Assert-RunRows $candidateEventRows $candidateProcessRows 'candidate' 'candidate'
$serial = Get-RunStatistics $serialEventRows $serialProcessRows 'serial'
$candidate = Get-RunStatistics $candidateEventRows $candidateProcessRows 'candidate'

$candidateBatches = @($candidateEventRows | Where-Object { $_.event -ceq 'CandidateBatch' })
$scopeRowsExact = @($candidateBatches | Where-Object {
  $_.batch_result -cne 'Success' -or $_.scope_begins -cne '1' -or
  $_.scope_finishes -cne '1' -or $_.scope_polls -cne '1'
}).Count -eq 0
$scopeTotalsExact = $candidate.scope_begins_total -eq $candidate.batch_count -and
  $candidate.scope_finishes_total -eq $candidate.batch_count -and
  $candidate.scope_polls_total -eq $candidate.batch_count

$visibleSerial = $serial.VisibleMeasurement
$visibleCandidate = $candidate.VisibleMeasurement
$visibleBatchCpuGate = Get-VisibleBatchCpu8msAndNoRegression $visibleSerial.batch_cpu_p95.value $visibleCandidate.batch_cpu_p95.value
$visibleScopeAmplificationGate = Get-VisibleScopeAmplificationReduced50Percent $visibleSerial $visibleCandidate
$minimizedPresentationPaused = Get-MinimizedPresentationPaused $serial $candidate
$phaseGates = Get-PhaseGates $serial $candidate
$serialRestoreReceipt = [bool]$serialRun.restore_identity_bearing_presentation_present
$candidateRestoreReceipt = [bool]$candidateRun.restore_identity_bearing_presentation_present
$restoreReceiptPresent = $serialRestoreReceipt -and $candidateRestoreReceipt

$report = [ordered]@{
  schema_version = 1
  serial = $serial
  candidate = $candidate
  predicates = [ordered]@{
    candidate_batches_success_scope_exact = $scopeRowsExact
    candidate_scope_totals_equal_batch_count = $scopeTotalsExact
    visible_batch_cpu_8ms_and_no_regression = $visibleBatchCpuGate
    visible_scope_amplification_reduced_50_percent = $visibleScopeAmplificationGate
    minimized_presentation_paused = $minimizedPresentationPaused
    phase = $phaseGates
    restore_identity_bearing_presentation_present = $restoreReceiptPresent
    restore_exact_color_and_working_input_requires_manual_evidence = $true
    fatal_no_present_requires_deterministic_fault_evidence = $true
  }
}

$mandatoryPredicates = [ordered]@{
  candidate_batches_success_scope_exact = $scopeRowsExact
  candidate_scope_totals_equal_batch_count = $scopeTotalsExact
  visible_batch_cpu_8ms_and_no_regression = $visibleBatchCpuGate
  visible_scope_amplification_reduced_50_percent = $visibleScopeAmplificationGate
  minimized_presentation_paused = $minimizedPresentationPaused
  restore_identity_bearing_presentation_present = $restoreReceiptPresent
}
foreach ($phase in $MeasuredPhases) {
  $gate = $phaseGates[$phase]
  $mandatoryPredicates["${phase}.cpu"] = $gate.cpu
  $mandatoryPredicates["${phase}.working_set_max"] = $gate.working_set_max
  $mandatoryPredicates["${phase}.working_set_trend"] = $gate.working_set_trend
  if ($gate.input_to_next_present_applicable) {
    $mandatoryPredicates["${phase}.input_to_next_present"] = $gate.input_to_next_present
  }
  if ($gate.frame_response_applicable) {
    $mandatoryPredicates["${phase}.frame_response"] = $gate.frame_response
  }
}
Write-ComparisonOutput $OutputPath $report $mandatoryPredicates
