param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('x86', 'x86_64', IgnoreCase = $false)]
  [string]$Architecture
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)
$target = switch ($architecture) {
  'x86_64' { 'x86_64-pc-windows-msvc' }
  'x86' { 'i686-pc-windows-msvc' }
  default { throw 'Private ABI build requires the fixed Windows x86 targets' }
}
$evidence = Join-Path $repo "target/rdp-publication-abi-windows-$Architecture"
$cargoOutput = Join-Path $repo "target/rdp-publication-abi-windows-build-$Architecture"
if (Test-Path -LiteralPath $evidence) { throw 'Refusing existing ABI evidence directory' }
[IO.Directory]::CreateDirectory($evidence) | Out-Null
$identityPath = Join-Path $evidence 'identity.json'
$builtMap = Join-Path $cargoOutput 'rdp_publication_abi.map'
$pePath = Join-Path $evidence 'rdp_publication_abi.exe'
$sources = @(
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/rdp_publication_abi.rs'; snapshot = 'source-rdp_publication_abi.rs' },
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/support/rdp_publication_abi_x64.rs'; snapshot = 'source-rdp_publication_abi_x64.rs' },
  @{ path = 'crates/frd-rdp-legacy-kernels/examples/support/rdp_publication_abi_i686.rs'; snapshot = 'source-rdp_publication_abi_i686.rs' },
  @{ path = 'crates/frd-protocol-rdp/src/pixel_convert/x86.rs'; snapshot = 'source-pixel_convert-x86.rs' }
)
$configNames = @('Cargo.toml', 'Cargo.lock', 'crates/frd-rdp-legacy-kernels/Cargo.toml', 'rust-toolchain.toml', '.cargo/config.toml', '.github/workflows/build-windows.yml', 'tools/verify-windows-publication-abi.ps1')
# 人工冻结的最终已审图像；不从本次 build 自行刷新。
$allowedSha256 = @{
  'i686-pc-windows-msvc' = 'ba2b29ad8f7a56b3e99bb93edd176aa188da4f4b845e08c859b91475ebe7dbc2'
  'x86_64-pc-windows-msvc' = '6120db8179d4d93bf8428601a256b1463e2244dc47a2cf0c2bcaf654fc5443e6'
}
function New-PrivateAbiRecord {
  param([ValidateSet('build', 'abi')][string]$Stage)
  return [ordered]@{
    stage = $Stage; executable = $null; argv = $null; budget_ms = $(if ($Stage -ceq 'build') { 900000 } else { 30000 })
    started = $false; terminal_observed = $false; timed_out = $false; exit_code = $null
    stdout_eof = $false; stderr_eof = $false; capture_complete = $false; streams_closed = $false
    cleanup = @(); errors = @(); failure_exit = 0; validation = 'NOT_RUN'; process_tree_quiescence = 'not proven'
  }
}
$identity = [ordered]@{
  schema = 'frd-publication-direct-abi-exact-image-runtime-v1'; commit = $env:GITHUB_SHA
  run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT; job = $env:GITHUB_JOB
  architecture = $architecture; target = $target; required_rust = '1.96.0'
  toolchain_precondition = 'preceding pinned toolchain step succeeded'
  host = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
  runtime = 'NOT_RUN'; runtime_complete = $false; control_flow_protection = 'not-qualified'; build_complete = $false
  admission = [ordered]@{ scope = 'explicit-root-private-default-runner-test'; expected_sha256 = $allowedSha256[$target]; actual_sha256 = $null; final_static_review_sha256 = '626ef2a0d69c08777bdee26c02d8c6c95fccdcf6465c0b40cf6daa767a8c2af9'; passed = $false }
  ownership_scope = 'direct Process only'; descendant_quiescence = 'NOT_PROVEN'
  linker_argv = 'not observed; Cargo -v is not the full actual linker argv'
  strip_status = 'not presumed; inherited release profile/config/environment'
  sources = @(); configuration = @(); tools = @(); artifacts = @(); errors = @(); build = (New-PrivateAbiRecord 'build'); abi = (New-PrivateAbiRecord 'abi')
  output_validation = [ordered]@{ passed = $false; expected_lines = $(if ($architecture -ceq 'x86') { 28 } else { 84 }); actual_lines = $null; reason = 'NOT_RUN' }
}
$firstExit = 0
$firstFailure = $null
function Invoke-PrivateAbiStage {
  param([ValidateSet('build', 'abi')][string]$Stage)
  # 闭集命令：不接受路径、argv 或预算覆盖。
  if ($Stage -ceq 'build') {
    $executable = $cargoPath
    $arguments = @('rustc', '--locked', '--release', '-p', 'frd-rdp-legacy-kernels', '--example', 'rdp_publication_abi', '--target', $target, '-v', '--', '-C', "link-arg=/MAP:$builtMap")
  }
  else { $executable = $pePath; $arguments = @('abi') }
  $record = $identity[$Stage]
  $record.executable = $executable; $record.argv = $arguments
  $budgetMs = $record.budget_ms
  $process = [Diagnostics.Process]::new()
  $stdoutFile = $null; $stderrFile = $null; $stdoutCopy = $null; $stderrCopy = $null
  $primary = $null
  try {
    $fileMode = if ($Stage -ceq 'build') { [IO.FileMode]::CreateNew } else { [IO.FileMode]::Open }
    $stdoutFile = [IO.FileStream]::new((Join-Path $evidence ($Stage + '.stdout')), $fileMode, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $stderrFile = [IO.FileStream]::new((Join-Path $evidence ($Stage + '.stderr')), $fileMode, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    if ($Stage -ceq 'abi' -and ($stdoutFile.Length -ne 0 -or $stderrFile.Length -ne 0)) { throw 'Refusing nonempty pre-launch ABI raw streams' }
    [IO.File]::WriteAllText($identityPath, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $executable; $start.WorkingDirectory = $repo
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true; $start.RedirectStandardInput = $true
    $start.Environment['CARGO_TARGET_DIR'] = $cargoOutput
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
function Confirm-PrivateAbiInputs {
  param([ValidateSet('after_build', 'before_abi', 'after_abi')][string]$Phase)
  if ($identity.sources.Count -ne 4 -or $identity.configuration.Count -ne $configNames.Count -or $identity.artifacts.Count -ne 2) { throw "Incomplete source/config/artifact identity at $Phase" }
  foreach ($source in $identity.sources) {
    $current = [ordered]@{
      source = (Get-FileHash -LiteralPath (Join-Path $repo $source.path) -Algorithm SHA256).Hash.ToLowerInvariant()
      snapshot = (Get-FileHash -LiteralPath (Join-Path $evidence $source.snapshot) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $source[$Phase] = $current
    if ($current.source -cne $source.before -or $current.snapshot -cne $source.before) { throw "Source or snapshot changed at ${Phase}: $($source.path)" }
  }
  foreach ($config in $identity.configuration) {
    $config[$Phase] = (Get-FileHash -LiteralPath (Join-Path $repo $config.path) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($config.before -cne $config[$Phase]) { throw "Configuration changed at ${Phase}: $($config.path)" }
  }
  foreach ($artifact in $identity.artifacts) {
    $current = [ordered]@{
      build = (Get-FileHash -LiteralPath $artifact.build_path -Algorithm SHA256).Hash.ToLowerInvariant()
      staged = (Get-FileHash -LiteralPath (Join-Path $evidence $artifact.path) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $artifact[$Phase] = $current
    if ($current.build -cne $artifact.build_sha256 -or $current.staged -cne $artifact.staged_sha256 -or $artifact.staged_sha256 -cne $artifact.build_sha256) { throw "Artifact changed at ${Phase}: $($artifact.kind)" }
  }
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
  # 初始 identity 为 partial；唯一 Cargo launch 前先建原始双流文件。
  [IO.File]::WriteAllText($identityPath, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
  foreach ($name in @('abi.stdout', 'abi.stderr')) {
    $empty = [IO.FileStream]::new((Join-Path $evidence $name), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $empty.Dispose()
  }
  if (Test-Path -LiteralPath $cargoOutput) { throw 'Refusing existing ABI Cargo output directory' }
  [IO.Directory]::CreateDirectory($cargoOutput) | Out-Null
  foreach ($source in $sources) {
    $path = Join-Path $repo $source.path
    $before = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    Copy-Item -LiteralPath $path -Destination (Join-Path $evidence $source.snapshot)
    $snapshotHash = (Get-FileHash -LiteralPath (Join-Path $evidence $source.snapshot) -Algorithm SHA256).Hash.ToLowerInvariant()
    $identity.sources += [ordered]@{ path = $source.path; snapshot = $source.snapshot; before = $before; snapshot_sha256 = $snapshotHash }
    if ($before -cne $snapshotHash) { throw "Source snapshot mismatch: $($source.path)" }
  }
  foreach ($name in $configNames) {
    $identity.configuration += [ordered]@{ path = $name; before = (Get-FileHash -LiteralPath (Join-Path $repo $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
  }
  $cargoPath = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
  $rustcPath = (Get-Command rustc -CommandType Application | Select-Object -First 1).Source
  if ([string]::IsNullOrWhiteSpace($cargoPath) -or [string]::IsNullOrWhiteSpace($rustcPath)) { throw 'Missing pinned Rust command paths' }
  foreach ($tool in @(@{ command = 'cargo'; path = $cargoPath }, @{ command = 'rustc'; path = $rustcPath })) {
    $identity.tools += [ordered]@{ command = $tool.command; path = $tool.path; sha256 = (Get-FileHash -LiteralPath $tool.path -Algorithm SHA256).Hash.ToLowerInvariant(); identity_scope = 'command file may be rustup shim; pin checked by preceding step' }
  }
  # 精选编译变量；不收集环境全集或凭据，也不改保护/LTO/strip配置。
  $identity.compilation_environment = [ordered]@{}
  foreach ($name in @('RUSTUP_TOOLCHAIN', 'RUSTUP_HOME', 'CARGO_HOME', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'CARGO_PROFILE_RELEASE_LTO', 'CARGO_PROFILE_RELEASE_STRIP', 'CARGO_PROFILE_RELEASE_DEBUG', 'CARGO_PROFILE_RELEASE_CODEGEN_UNITS', 'CARGO_PROFILE_RELEASE_OPT_LEVEL', 'CARGO_PROFILE_RELEASE_PANIC', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', 'CARGO_TARGET_I686_PC_WINDOWS_MSVC_LINKER', 'CARGO_TARGET_I686_PC_WINDOWS_MSVC_RUSTFLAGS')) {
    $identity.compilation_environment[$name] = [Environment]::GetEnvironmentVariable($name)
  }
  $identity.compilation_environment['CARGO_TARGET_DIR'] = $cargoOutput
  [IO.File]::WriteAllText($identityPath, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
  # 固定 build stage；仅完整 build 和精确已审图像准入后才执行 abi stage。
  $result = Invoke-PrivateAbiStage 'build'
  $identity.build = $result
  if ($result.failure_exit -ne 0) {
    $firstExit = $result.failure_exit
    throw "ABI build failed: $($result.errors -join '; ') native_exit=$($result.exit_code) capture_complete=$($result.capture_complete)"
  }
  $built = Join-Path $cargoOutput "$target/release/examples/rdp_publication_abi.exe"
  foreach ($artifact in @(@{ kind = 'pe'; original = $built; name = 'rdp_publication_abi.exe' }, @{ kind = 'link_map'; original = $builtMap; name = 'rdp_publication_abi.map' })) {
    if ((Get-Item -LiteralPath $artifact.original).Length -eq 0) { throw "Empty build artifact: $($artifact.kind)" }
    $row = [ordered]@{ kind = $artifact.kind; build_path = $artifact.original; path = $artifact.name; build_sha256 = (Get-FileHash -LiteralPath $artifact.original -Algorithm SHA256).Hash.ToLowerInvariant(); staged_sha256 = $null }
    $identity.artifacts += $row
    Copy-Item -LiteralPath $artifact.original -Destination (Join-Path $evidence $artifact.name)
    $row.staged_sha256 = (Get-FileHash -LiteralPath (Join-Path $evidence $artifact.name) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($row.build_sha256 -cne $row.staged_sha256) { throw "Staged artifact mismatch: $($artifact.kind)" }
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
  $expectedMachine = if ($architecture -eq 'x86') { 0x014c } else { 0x8664 }
  $expectedMagic = if ($architecture -eq 'x86') { 0x010b } else { 0x020b }
  if ($optionalSize -lt 2 -or ($peOffset + 24 + $optionalSize) -gt $bytes.Length -or $machine -ne $expectedMachine -or $magic -ne $expectedMagic) { throw 'Private ABI PE target/header mismatch' }
  $identity.pe_header = [ordered]@{ bytes = $length; machine = ('0x{0:X4}' -f $machine); optional_magic = ('0x{0:X4}' -f $magic) }
  Confirm-PrivateAbiInputs 'after_build'
  $identity.build_complete = $true
  $identity.build.validation = 'native-exit-zero-and-build-identities'
}
catch {
  $identity.errors += $_.Exception.ToString()
  if ($firstExit -eq 0) { $firstExit = 1 }
  if ($null -eq $firstFailure) { $firstFailure = $_.Exception.ToString() }
}
if (-not $identity.build_complete -and $firstExit -eq 0) { $firstExit = 1; $firstFailure = 'Build evidence remained partial' }
try {
  if ($identity.build_complete -and $firstExit -eq 0) {
    Confirm-PrivateAbiInputs 'before_abi'
    $actualHash = (Get-FileHash -LiteralPath $pePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $identity.admission.actual_sha256 = $actualHash
    if ($actualHash -cne $allowedSha256[$target]) { throw 'Exact reviewed staged PE SHA mismatch; runtime NOT_RUN; no automatic admission refresh' }
    $identity.admission.passed = $true
    $identity.runtime = 'PARTIAL'
    $result = Invoke-PrivateAbiStage 'abi'
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
    Confirm-PrivateAbiInputs 'after_abi'
    $identity.runtime_complete = $firstExit -eq 0 -and $result.capture_complete -and $result.exit_code -eq 0 -and $identity.output_validation.passed
    if ($identity.runtime_complete) { $identity.runtime = 'PASS' }
  }
}
catch {
  $identity.errors += $_.Exception.ToString()
  if ($firstExit -eq 0) { $firstExit = 1; $firstFailure = $_.Exception.ToString() }
}
if ($identity.runtime -ceq 'NOT_RUN') { $identity.abi['skip_reason'] = if ($null -ne $firstFailure) { $firstFailure } else { 'build or exact-image gate did not complete' } }
if (-not $identity.runtime_complete -and $firstExit -eq 0) { $firstExit = 1; $firstFailure = 'Private exact-image ABI runtime remained partial' }
$identity.first_failure = $firstFailure; $identity.step_exit = $firstExit
try {
  $pendingIdentity = Join-Path $evidence 'identity.pending.json'
  [IO.File]::WriteAllText($pendingIdentity, (($identity | ConvertTo-Json -Depth 12) + "`n"), [Text.UTF8Encoding]::new($false))
  [IO.File]::Move($pendingIdentity, $identityPath, $true)
}
catch {
  [Console]::Error.WriteLine('Identity finalization failed: ' + $_.Exception.ToString())
  if ($firstExit -eq 0) { $firstExit = 1; $firstFailure = $_.Exception.ToString() }
}
if ($firstExit -ne 0) { [Console]::Error.WriteLine($firstFailure); exit $firstExit }
