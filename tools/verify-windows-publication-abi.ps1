param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('x86', 'x86_64', IgnoreCase = $false)]
  [string]$Architecture
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)
$target = switch ($Architecture) {
  'x86_64' { 'x86_64-pc-windows-msvc' }
  'x86' { 'i686-pc-windows-msvc' }
  default { throw 'Private ABI requires the fixed Windows x86 targets' }
}
$sourceInput = Join-Path $repo "target/rdp-publication-abi-source-windows-$Architecture"
$evidence = Join-Path $repo "target/rdp-publication-abi-windows-$Architecture"
if (Test-Path -LiteralPath $evidence) { throw 'Refusing existing ABI evidence directory' }
[IO.Directory]::CreateDirectory($evidence) | Out-Null
$identityPath = Join-Path $evidence 'runtime.identity.json'
$pePath = Join-Path $evidence 'rdp_publication_abi.exe'
$sourceRunId = '38880607150'
$sourceCommit = 'f5d9ae50b31e0f89de9d0943065bd9241ef2a4f5'
$sourceArtifactId = if ($Architecture -ceq 'x86') { '12037710605' } else { '12036449351' }
$sources = @(
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/rdp_publication_abi.rs'; snapshot = 'source-rdp_publication_abi.rs' },
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/support/rdp_publication_abi_x64.rs'; snapshot = 'source-rdp_publication_abi_x64.rs' },
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/support/rdp_publication_abi_i686.rs'; snapshot = 'source-rdp_publication_abi_i686.rs' },
  @{ path = 'crates/frd-protocol-rdp/src/pixel_convert/x86.rs'; snapshot = 'source-pixel_convert-x86.rs' }
)
$buildConfigNames = @('Cargo.toml', 'Cargo.lock', 'crates/frd-rdp-legacy-kernels/Cargo.toml', 'rust-toolchain.toml', '.cargo/config.toml')
$configNames = $buildConfigNames + @('.github/workflows/build-windows.yml', 'tools/verify-windows-publication-abi.ps1')
# 完整 PE 准入保持；不归一化、不重建追逐旧 hash。
$allowedSha256 = @{
  'i686-pc-windows-msvc' = 'ba2b29ad8f7a56b3e99bb93edd176aa188da4f4b845e08c859b91475ebe7dbc2'
  'x86_64-pc-windows-msvc' = '6120db8179d4d93bf8428601a256b1463e2244dc47a2cf0c2bcaf654fc5443e6'
}
$sourceMembers = if ($Architecture -ceq 'x86') {
  [ordered]@{
    'source-rdp_publication_abi.rs' = '84b36f1fed278382aa21a5081a3346973b6dcb2f5e29e72192f34a330b79dcc3'
    'source-rdp_publication_abi_x64.rs' = '7434158ce9b432a539235d0c4ef22b6ff45dd3796118c77ad82e3abe70d59707'
    'source-rdp_publication_abi_i686.rs' = '750d3ffdc9c97d466cabf02da084ca68bd70ff890eac6e50e2a0bfaa8f68e1ea'
    'source-pixel_convert-x86.rs' = '232fabaca6781a9e3ef5110de292d3c4b017066b847f7ee9b31ad4f7ad8aeaf9'
    'rdp_publication_abi.exe' = 'ba2b29ad8f7a56b3e99bb93edd176aa188da4f4b845e08c859b91475ebe7dbc2'
    'rdp_publication_abi.map' = 'd1214a40db915246995ffb5565fed9f48f5109cd7d451664e3b4bfe0373501df'
    'identity.json' = 'a7e2f8e8ff6fc315a0bb6add429a83a2f9c37d47b6b9a63bef8c20ea84cb0ddb'
    'build.stdout' = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    'build.stderr' = 'aa66848a512b83953e4c385002871fe24d1f2524d6d9c4f13be21789ac84006a'
  }
}
else {
  [ordered]@{
    'source-rdp_publication_abi.rs' = '84b36f1fed278382aa21a5081a3346973b6dcb2f5e29e72192f34a330b79dcc3'
    'source-rdp_publication_abi_x64.rs' = '7434158ce9b432a539235d0c4ef22b6ff45dd3796118c77ad82e3abe70d59707'
    'source-rdp_publication_abi_i686.rs' = '750d3ffdc9c97d466cabf02da084ca68bd70ff890eac6e50e2a0bfaa8f68e1ea'
    'source-pixel_convert-x86.rs' = '232fabaca6781a9e3ef5110de292d3c4b017066b847f7ee9b31ad4f7ad8aeaf9'
    'rdp_publication_abi.exe' = '6120db8179d4d93bf8428601a256b1463e2244dc47a2cf0c2bcaf654fc5443e6'
    'rdp_publication_abi.map' = '2d53ddeaf52758adba5f7c2c2f0147afce980f91bf61801aa0a4089ef3fc8958'
    'identity.json' = '6c03dab96400685176e8dca46fd47e637750a2124eef44bcb9249cc55e356575'
    'build.stdout' = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    'build.stderr' = 'a9f90df3f979e9afe74e215dcbe1d843a374c80484aa319ed4a73d29c851c8fb'
  }
}
function New-PrivateAbiRecord {
  return [ordered]@{
    stage = 'abi'; executable = $null; argv = $null; budget_ms = 30000
    started = $false; terminal_observed = $false; timed_out = $false; exit_code = $null
    stdout_eof = $false; stderr_eof = $false; capture_complete = $false; streams_closed = $false
    cleanup = @(); errors = @(); failure_exit = 0; validation = 'NOT_RUN'; process_tree_quiescence = 'not proven'
  }
}
$identity = [ordered]@{
  schema = 'frd-publication-direct-abi-reused-image-runtime-v1'; commit = $env:GITHUB_SHA
  run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT; job = $env:GITHUB_JOB
  architecture = $Architecture; target = $target
  host = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
  build_reused = $true; abi_build_in_current_run = 'NOT_RUN'; source_build_complete = $false
  source_repository = 'sunanxin18/FreeRemoteAccess'; source_run_id = $sourceRunId; source_run_attempt = '1'
  source_commit = $sourceCommit; source_artifact_id = $sourceArtifactId; source_identity_sha256 = $sourceMembers['identity.json']
  source_build_record_scope = 'old identity.json/build streams are source-build originals; no old PID or native exit belongs to this run'
  runtime = 'NOT_RUN'; runtime_complete = $false; control_flow_protection = 'not-qualified'
  admission = [ordered]@{ scope = 'explicit-root-private-default-runner-test'; expected_sha256 = $allowedSha256[$target]; actual_sha256 = $null; final_static_review_sha256 = '626ef2a0d69c08777bdee26c02d8c6c95fccdcf6465c0b40cf6daa767a8c2af9'; passed = $false }
  ownership_scope = 'direct Process only'; descendant_quiescence = 'NOT_PROVEN'
  current_sources = @(); current_configuration = @(); file_checks = [ordered]@{}; errors = @(); abi = (New-PrivateAbiRecord)
  output_validation = [ordered]@{ passed = $false; expected_lines = $(if ($Architecture -ceq 'x86') { 28 } else { 84 }); actual_lines = $null; reason = 'NOT_RUN' }
}
$firstExit = 0
$firstFailure = $null
function Invoke-PrivateAbiStage {
  $Stage = 'abi'
  $executable = $pePath; $arguments = @('abi')
  $record = $identity[$Stage]
  $record.executable = $executable; $record.argv = $arguments
  $budgetMs = $record.budget_ms
  $process = [Diagnostics.Process]::new()
  $stdoutFile = $null; $stderrFile = $null; $stdoutCopy = $null; $stderrCopy = $null
  $primary = $null
  try {
    $fileMode = [IO.FileMode]::Open
    $stdoutFile = [IO.FileStream]::new((Join-Path $evidence ($Stage + '.stdout')), $fileMode, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $stderrFile = [IO.FileStream]::new((Join-Path $evidence ($Stage + '.stderr')), $fileMode, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    if ($Stage -ceq 'abi' -and ($stdoutFile.Length -ne 0 -or $stderrFile.Length -ne 0)) { throw 'Refusing nonempty pre-launch ABI raw streams' }
    [IO.File]::WriteAllText($identityPath, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $executable; $start.WorkingDirectory = $repo
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true; $start.RedirectStandardInput = $true
    foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
    $process.StartInfo = $start
    if (-not $process.Start()) { throw 'Process.Start returned false' }
    $record.started = $true; $record.pid = $process.Id
    $process.StandardInput.Close()
    # 原 BaseStream 到 FileStream：不经 PowerShell 文本管线或重新编码。
    $stdoutCopy = $process.StandardOutput.BaseStream.CopyToAsync($stdoutFile)
    $stderrCopy = $process.StandardError.BaseStream.CopyToAsync($stderrFile)
    if (-not $process.WaitForExit($budgetMs)) {
      $record.timed_out = $true; $record.failure_exit = 124
      throw ('Direct ' + $Stage + ' process exceeded fixed budget')
    }
    $record.terminal_observed = $true; $record.exit_code = $process.ExitCode
    if ($record.exit_code -ne 0) { $record.failure_exit = $record.exit_code }
  }
  catch {
    $primary = $_.Exception.ToString()
    if ($record.failure_exit -eq 0) { $record.failure_exit = 1 }
  }
  finally {
    # 只终止手中这个 Process；后代不在此所有权证明内。
    if ($record.started -and -not $record.terminal_observed) {
      try { $process.Kill(); $record.cleanup += 'direct Kill returned' }
      catch { $record.errors += "direct Kill: $($_.Exception.ToString())" }
      try {
        if ($process.WaitForExit(1000)) {
          $record.terminal_observed = $true; $record.exit_code = $process.ExitCode
          $record.cleanup += 'direct terminal observed within 1s'
        }
        else { $record.errors += 'direct terminal remains unknown after 1s' }
      }
      catch { $record.errors += "direct reap: $($_.Exception.ToString())" }
    }
    # 即使 direct terminal 已知，也不假定后代关闭了继承写端。EOF 另有2s预算。
    if ($null -ne $stdoutCopy -and $null -ne $stderrCopy) {
      try {
        $copies = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($stdoutCopy, $stderrCopy))
        if (-not $copies.Wait(2000)) { $record.errors += 'raw stream EOF remains unknown after 2s' }
      }
      catch { $record.errors += "raw stream capture: $($_.Exception.ToString())" }
      $record.stdout_eof = $stdoutCopy.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion
      $record.stderr_eof = $stderrCopy.Status -eq [Threading.Tasks.TaskStatus]::RanToCompletion
    }
    $filesClosed = $true
    foreach ($pair in @(@{ stream = $stdoutFile; copy = $stdoutCopy; name = 'stdout' }, @{ stream = $stderrFile; copy = $stderrCopy; name = 'stderr' })) {
      if ($null -eq $pair.stream) { $filesClosed = $false; continue }
      if ($null -ne $pair.copy -and -not $pair.copy.IsCompleted) {
        # 不无限等待仍在写的 task，也不将 partial 文件称为稳定原件。
        $filesClosed = $false; $record.errors += "$($pair.name) file remains open: capture task incomplete"
        continue
      }
      try {
        $dispose = $pair.stream.DisposeAsync().AsTask()
        if (-not $dispose.Wait(2000)) { $filesClosed = $false; $record.errors += "$($pair.name) file disposal exceeds 2s" }
      }
      catch { $filesClosed = $false; $record.errors += "$($pair.name) file disposal: $($_.Exception.ToString())" }
    }
    if (-not $record.started -or ($record.terminal_observed -and ($null -eq $stdoutCopy -or $stdoutCopy.IsCompleted) -and ($null -eq $stderrCopy -or $stderrCopy.IsCompleted))) {
      try { $process.Dispose() }
      catch { $record.errors += "Process.Dispose: $($_.Exception.ToString())" }
    }
    # 未知终态/EOF/收尾 IO 一律 partial；不会把 logger 的0覆盖原始非0。
    if ($null -ne $primary) { $record.errors = @($primary) + $record.errors }
    $record.streams_closed = $filesClosed
    $record.capture_complete = $record.started -and -not $record.timed_out -and $record.terminal_observed -and $record.stdout_eof -and $record.stderr_eof -and $filesClosed -and $record.errors.Count -eq 0
    if (-not $record.capture_complete -and $record.failure_exit -eq 0) { $record.failure_exit = 1 }
    if ($record.errors.Count -ne 0 -and $record.failure_exit -eq 0) { $record.failure_exit = 1 }
  }
  $record.validation = if ($record.capture_complete -and $record.exit_code -eq 0) { 'native-exit-zero' } else { 'FAILED' }
  return $record
}
function Confirm-OriginalAbiFiles {
  param([ValidateSet('source', 'evidence')][string]$Scope)
  $directory = if ($Scope -ceq 'source') { $sourceInput } else { $evidence }
  $rootItem = Get-Item -LiteralPath $directory -Force
  if (-not $rootItem.PSIsContainer -or ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not [string]::IsNullOrEmpty([string]$rootItem.LinkType)) { throw "Refusing non-directory or linked $Scope input" }
  $expectedNames = @($sourceMembers.Keys)
  if ($Scope -ceq 'evidence') { $expectedNames += @('runtime.identity.json', 'abi.stdout', 'abi.stderr') }
  $entries = @(Get-ChildItem -LiteralPath $directory -Force)
  if ($entries.Count -ne $expectedNames.Count) { throw "Unexpected flat member count in $Scope" }
  foreach ($entry in $entries) {
    if ($expectedNames -cnotcontains $entry.Name -or $entry -isnot [IO.FileInfo] -or $entry.PSIsContainer -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or -not [string]::IsNullOrEmpty([string]$entry.LinkType)) { throw "Unexpected member or linked/directory substitute in ${Scope}: $($entry.Name)" }
  }
  $hashes = [ordered]@{}
  foreach ($name in $sourceMembers.Keys) {
    $hash = (Get-FileHash -LiteralPath (Join-Path $directory $name) -Algorithm SHA256).Hash.ToLowerInvariant()
    $hashes[$name] = $hash
    if ($hash -cne $sourceMembers[$name]) { throw "Pinned original member SHA mismatch in ${Scope}: $name" }
  }
  return $hashes
}
function Confirm-SourceBuild {
  $original = Get-Content -LiteralPath (Join-Path $evidence 'identity.json') -Raw | ConvertFrom-Json
  if ($original.schema -cne 'frd-publication-direct-abi-build-only-v1' -or $original.commit -cne $sourceCommit -or [string]$original.run_id -cne $sourceRunId -or [string]$original.run_attempt -cne '1' -or $original.job -cne 'package' -or $original.architecture -cne $Architecture -or $original.target -cne $target -or $original.required_rust -cne '1.96.0') { throw 'Pinned source build origin/target mismatch' }
  if ($original.build_complete -ne $true -or $original.step_exit -ne 0 -or @($original.errors).Count -ne 0 -or $null -ne $original.first_failure -or $original.runtime -cne 'NOT_RUN' -or $original.control_flow_protection -cne 'not-qualified') { throw 'Pinned source build verdict is not complete' }
  $build = $original.build
  if ($build.stage -cne 'build' -or $build.started -ne $true -or $build.terminal_observed -ne $true -or $build.timed_out -ne $false -or $build.exit_code -ne 0 -or $build.stdout_eof -ne $true -or $build.stderr_eof -ne $true -or $build.capture_complete -ne $true -or $build.failure_exit -ne 0 -or @($build.errors).Count -ne 0) { throw 'Pinned source native build/capture was not complete zero' }
  if (@($original.sources).Count -ne 4) { throw 'Pinned source build must identify four source files' }
  foreach ($source in $sources) {
    $rows = @($original.sources | Where-Object { $_.path -ceq $source.path -and $_.snapshot -ceq $source.snapshot })
    if ($rows.Count -ne 1) { throw "Pinned source identity row mismatch: $($source.path)" }
    $expected = $sourceMembers[$source.snapshot]
    foreach ($field in @('before', 'snapshot_sha256', 'after', 'snapshot_after')) {
      if ($rows[0].$field -cne $expected) { throw "Pinned source build hash mismatch: $($source.path) $field" }
    }
    if ((Get-FileHash -LiteralPath (Join-Path $repo $source.path) -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expected) { throw "Current source differs from reviewed source: $($source.path)" }
  }
  $originalConfigNames = $buildConfigNames + @('.github/workflows/build-windows.yml')
  if (@($original.configuration).Count -ne 6) { throw 'Pinned source build must identify six original configurations' }
  foreach ($name in $originalConfigNames) {
    $rows = @($original.configuration | Where-Object { $_.path -ceq $name })
    if ($rows.Count -ne 1 -or $rows[0].before -cne $rows[0].after) { throw "Pinned build configuration row mismatch: $name" }
    if ($name -ceq '.github/workflows/build-windows.yml') {
      # 旧 workflow provenance 保留；不要求它等于当前运输/执行接线。
      if ($rows[0].before -cne '634f75871a023f6b1ca57844372285fe17f51ad6dfb4474ed770cb35a5b27446') { throw 'Pinned original workflow identity mismatch' }
    }
    elseif ((Get-FileHash -LiteralPath (Join-Path $repo $name) -Algorithm SHA256).Hash.ToLowerInvariant() -cne $rows[0].before) { throw "Current substantive build configuration differs: $name" }
  }
  if (@($original.artifacts).Count -ne 2) { throw 'Pinned source build must identify PE and same-build map' }
  foreach ($name in @('rdp_publication_abi.exe', 'rdp_publication_abi.map')) {
    $rows = @($original.artifacts | Where-Object { $_.path -ceq $name })
    if ($rows.Count -ne 1) { throw "Pinned build artifact row mismatch: $name" }
    foreach ($field in @('build_sha256', 'staged_sha256', 'build_after', 'staged_final')) {
      if ($rows[0].$field -cne $sourceMembers[$name]) { throw "Pinned build artifact identity mismatch: $name $field" }
    }
  }
  $identity.source_build_complete = $true
}
function Confirm-CurrentAbiInputs {
  param([ValidateSet('before_abi', 'after_abi')][string]$Phase)
  if ($identity.current_sources.Count -ne 4 -or $identity.current_configuration.Count -ne 7) { throw "Current source/configuration baseline incomplete at $Phase" }
  foreach ($source in $identity.current_sources) {
    $source[$Phase] = (Get-FileHash -LiteralPath (Join-Path $repo $source.path) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($source[$Phase] -cne $source.before -or $source.before -cne $sourceMembers[$source.snapshot]) { throw "Current source changed at ${Phase}: $($source.path)" }
  }
  foreach ($config in $identity.current_configuration) {
    $config[$Phase] = (Get-FileHash -LiteralPath (Join-Path $repo $config.path) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($config[$Phase] -cne $config.before) { throw "Current configuration changed at ${Phase}: $($config.path)" }
  }
  $identity.file_checks[$Phase] = [ordered]@{ source = (Confirm-OriginalAbiFiles 'source'); evidence = (Confirm-OriginalAbiFiles 'evidence') }
}
function Get-ExpectedAbiLines {
  param([string]$Target)
  # 人工转录固定三表，Windows x64 保持先全16项、再附加24项的源码顺序。
  $state = @('sp_ret8:00040000', 'df:00080000', 'mxcsr:00100000', 'x87cw:00200000')
  switch -CaseSensitive ($Target) {
    { $_ -cin @('i686-unknown-linux-gnu', 'i686-pc-windows-msvc') } {
      $controls = @('ebx_low:00000001', 'ebx_high:00000001', 'ebp_low:00000002', 'ebp_high:00000002', 'esi_low:00000004', 'esi_high:00000004', 'edi_low:00000008', 'edi_high:00000008') + $state
    }
    { $_ -cin @('x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc') } {
      $controls = @('rbx_low:00000001', 'rbx_high:00000001', 'rbp_low:00000002', 'rbp_high:00000002', 'r12_low:00000004', 'r12_high:00000004', 'r13_low:00000008', 'r13_high:00000008', 'r14_low:00000010', 'r14_high:00000010', 'r15_low:00000020', 'r15_high:00000020') + $state
    }
    default { throw 'Unknown ABI output target' }
  }
  if ($Target -ceq 'x86_64-pc-windows-msvc') {
    $controls += @('rsi_low:00000040', 'rsi_high:00000040', 'rdi_low:00000080', 'rdi_high:00000080',
      'xmm6_low:00000100', 'xmm6_high:00000100', 'xmm7_low:00000200', 'xmm7_high:00000200',
      'xmm8_low:00000400', 'xmm8_high:00000400', 'xmm9_low:00000800', 'xmm9_high:00000800',
      'xmm10_low:00001000', 'xmm10_high:00001000', 'xmm11_low:00002000', 'xmm11_high:00002000',
      'xmm12_low:00004000', 'xmm12_high:00004000', 'xmm13_low:00008000', 'xmm13_high:00008000',
      'xmm14_low:00010000', 'xmm14_high:00010000', 'xmm15_low:00020000', 'xmm15_high:00020000')
  }
  $lines = [Collections.Generic.List[string]]::new()
  $lines.Add('ABI_PRECONDITION protection_configuration=external-admission-required final_copyfn_chain=external-review-required')
  foreach ($kernel in @('sse2', 'ssse3')) {
    foreach ($control in $controls) {
      $parts = $control.Split(':')
      $lines.Add("ABI_CALIBRATION kernel=$kernel case=$($parts[0]) exact_mask=0x$($parts[1])")
    }
    $lines.Add("ABI_VALID kernel=$kernel cases=9 exact_mask=0")
  }
  $lines.Add("ABI_OK scope=private-address-taken-normal-return controls_per_kernel=$($controls.Count) kernels=2 full_unwind=open shipping_fastcc_inline=open control_flow_protection=not-qualified protection_admission=external-required")
  return $lines.ToArray()
}
function Test-PrivateAbiOutput {
  param([string]$Target, [byte[]]$Raw, [byte[]]$RawStderr)
  $expected = @(Get-ExpectedAbiLines $Target)
  $result = [ordered]@{ passed = $false; expected_lines = $expected.Count; actual_lines = $null; reason = $null }
  if ($RawStderr.Length -ne 0) { $result.reason = 'successful ABI stderr must be empty'; return $result }
  foreach ($value in $Raw) {
    if ($value -gt 127 -or $value -eq 0) { $result.reason = 'stdout is not ASCII or contains NUL'; return $result }
  }
  $text = [Text.Encoding]::ASCII.GetString($Raw)
  if (-not $text.EndsWith("`n", [StringComparison]::Ordinal)) { $result.reason = 'missing final newline'; return $result }
  if ($text.Contains("`r")) {
    $unpaired = $text.Replace("`r`n", '')
    if ($unpaired.Contains("`r") -or $unpaired.Contains("`n")) { $result.reason = 'mixed or invalid newlines'; return $result }
    $text = $text.Replace("`r`n", "`n")
  }
  $lines = $text.Substring(0, $text.Length - 1).Split([char]10)
  $result.actual_lines = $lines.Length
  if ($lines.Length -ne $expected.Count) { $result.reason = 'stdout differs from exact ordered closed output'; return $result }
  for ($index = 0; $index -lt $expected.Count; $index++) {
    if ($lines[$index] -cne $expected[$index]) { $result.reason = 'stdout differs from exact ordered closed output'; return $result }
  }
  $result.passed = $true; $result.reason = 'exact ordered closed output'
  return $result
}
try {
  # 当前 step 不编译 ABI 图像；先建 runtime partial 和两路原始文件，不覆盖旧 identity/build 日志。
  [IO.File]::WriteAllText($identityPath, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
  foreach ($name in @('abi.stdout', 'abi.stderr')) {
    $empty = [IO.FileStream]::new((Join-Path $evidence $name), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $empty.Dispose()
  }
  $identity.file_checks['before_copy'] = Confirm-OriginalAbiFiles 'source'
  foreach ($name in $sourceMembers.Keys) {
    [IO.File]::Copy((Join-Path $sourceInput $name), (Join-Path $evidence $name), $false)
  }
  $identity.file_checks['after_copy'] = [ordered]@{ source = (Confirm-OriginalAbiFiles 'source'); evidence = (Confirm-OriginalAbiFiles 'evidence') }
  Confirm-SourceBuild
  foreach ($source in $sources) {
    $identity.current_sources += [ordered]@{ path = $source.path; snapshot = $source.snapshot; before = (Get-FileHash -LiteralPath (Join-Path $repo $source.path) -Algorithm SHA256).Hash.ToLowerInvariant() }
  }
  foreach ($name in $configNames) {
    $identity.current_configuration += [ordered]@{ path = $name; before = (Get-FileHash -LiteralPath (Join-Path $repo $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
  }
  $length = (Get-Item -LiteralPath $pePath).Length
  if ($length -lt 64 -or $length -gt 67108864) { throw 'Unexpected private ABI PE length' }
  $bytes = [IO.File]::ReadAllBytes($pePath)
  if ([BitConverter]::ToUInt16($bytes, 0) -ne 0x5a4d) { throw 'Private ABI PE lacks MZ' }
  $peOffset = [BitConverter]::ToUInt32($bytes, 0x3c)
  if ($peOffset -gt ($bytes.Length - 26) -or [BitConverter]::ToUInt32($bytes, $peOffset) -ne 0x4550) { throw 'Invalid private ABI PE signature/range' }
  $machine = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
  $optionalSize = [BitConverter]::ToUInt16($bytes, $peOffset + 20)
  $magic = [BitConverter]::ToUInt16($bytes, $peOffset + 24)
  $expectedMachine = if ($Architecture -ceq 'x86') { 0x014c } else { 0x8664 }
  $expectedMagic = if ($Architecture -ceq 'x86') { 0x010b } else { 0x020b }
  if ($optionalSize -lt 2 -or ($peOffset + 24 + $optionalSize) -gt $bytes.Length -or $machine -ne $expectedMachine -or $magic -ne $expectedMagic) { throw 'Private ABI PE target/header mismatch' }
  $identity.pe_header = [ordered]@{ bytes = $length; machine = ('0x{0:X4}' -f $machine); optional_magic = ('0x{0:X4}' -f $magic) }
  Confirm-CurrentAbiInputs 'before_abi'
  $actualHash = (Get-FileHash -LiteralPath $pePath -Algorithm SHA256).Hash.ToLowerInvariant()
  $identity.admission.actual_sha256 = $actualHash
  if ($actualHash -cne $allowedSha256[$target]) { throw 'Exact reviewed reused PE SHA mismatch; runtime NOT_RUN; no automatic admission refresh' }
  $identity.admission.passed = $true
  $identity.runtime = 'PARTIAL'
  $result = Invoke-PrivateAbiStage
  if ($result.failure_exit -ne 0) {
    if ($result.exit_code -eq 77) { $identity.runtime = 'UNAVAILABLE' }
    $identity.errors += "ABI native exit/capture failed: $($result.exit_code)"
    if ($firstExit -eq 0) { $firstExit = $result.failure_exit; $firstFailure = "ABI native exit/capture failed: $($result.exit_code)" }
  }
  if ($result.capture_complete -and $result.exit_code -eq 0) {
    $identity.output_validation = Test-PrivateAbiOutput -Target $target -Raw ([IO.File]::ReadAllBytes((Join-Path $evidence 'abi.stdout'))) -RawStderr ([IO.File]::ReadAllBytes((Join-Path $evidence 'abi.stderr')))
    if (-not $identity.output_validation.passed) {
      $result.validation = 'FAILED'
      $identity.errors += $identity.output_validation.reason
      if ($firstExit -eq 0) { $firstExit = 1; $firstFailure = $identity.output_validation.reason }
    }
    else { $result.validation = 'exact-closed-output' }
  }
  Confirm-CurrentAbiInputs 'after_abi'
  $identity.runtime_complete = $firstExit -eq 0 -and $result.capture_complete -and $result.exit_code -eq 0 -and $identity.output_validation.passed
  if ($identity.runtime_complete) { $identity.runtime = 'PASS' }
}
catch {
  $identity.errors += $_.Exception.ToString()
  if ($firstExit -eq 0) { $firstExit = 1; $firstFailure = $_.Exception.ToString() }
}
if ($identity.runtime -ceq 'NOT_RUN') { $identity.abi['skip_reason'] = if ($null -ne $firstFailure) { $firstFailure } else { 'pinned source or exact-image gate did not complete' } }
if (-not $identity.runtime_complete -and $firstExit -eq 0) { $firstExit = 1; $firstFailure = 'Private reused-image ABI runtime remained partial' }
$identity.first_failure = $firstFailure; $identity.step_exit = $firstExit
try {
  $pendingIdentity = Join-Path $evidence 'runtime.identity.pending.json'
  [IO.File]::WriteAllText($pendingIdentity, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
  [IO.File]::Move($pendingIdentity, $identityPath, $true)
}
catch {
  [Console]::Error.WriteLine('Runtime identity finalization failed: ' + $_.Exception.ToString())
  if ($firstExit -eq 0) { $firstExit = 1; $firstFailure = $_.Exception.ToString() }
}
if ($firstExit -ne 0) { [Console]::Error.WriteLine($firstFailure); exit $firstExit }
