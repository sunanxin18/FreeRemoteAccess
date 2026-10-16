[CmdletBinding()]
param([ValidateSet('Execute', 'Collect', 'BootstrapDiagnostic', 'BootstrapCheckoutDiagnostic', 'BootstrapAptDiagnostic')][string]$Action = 'Execute')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

function Write-ReleaseAJson($Path, $Value) {
    $text = ($Value | ConvertTo-Json -Depth 24) + "`n"
    [IO.File]::WriteAllText($Path, $text, [Text.UTF8Encoding]::new($false))
}
function Write-ReleaseALf($Path, [string]$Text) {
    [IO.File]::WriteAllText($Path, $Text.Replace("`r`n", "`n").TrimEnd([char[]]"`r`n") + "`n", [Text.UTF8Encoding]::new($false))
}
function Read-ReleaseAJson($Path) { return Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json -AsHashtable }
function Add-ReleaseAError($State, [string]$Stage, [string]$Message) {
    $State.errors[$Stage] = @($State.errors[$Stage]) + @($Message)
    if ($null -eq $State.first_error) { $State.first_error = @{ stage = $Stage; message = $Message } }
}
function Test-ReleaseAAuthority([string]$EventName, [string]$Ref, [bool]$Execute) {
    return ($EventName -ceq 'workflow_dispatch' -and $Execute) -or
        ($EventName -ceq 'push' -and $Ref -ceq 'refs/heads/codex/rdp-egfx-hevc-preference')
}
function Get-ReleaseASupervisorSource([string]$Repo) {
    $text = [IO.File]::ReadAllText((Join-Path $Repo 'tools/build-ffmpeg-windows.ps1'))
    $matches = [regex]::Matches($text, '(?s)\$lifecycleSource = @''\r?\n(.*?)\r?\n''@')
    if ($matches.Count -ne 1) { throw '固定inline supervisor边界不唯一' }
    return $matches[0].Groups[1].Value
}
function Save-ReleaseAExecutable([string]$Name, [string]$Path, [string]$Destination) {
    $absolute = [IO.Path]::GetFullPath($Path)
    $before = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::Copy($absolute, $Destination, $false)
    $saved = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    $after = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($before -cne $saved -or $before -cne $after) { throw 'Windows executable读取期间变化；保留partial并拒绝身份' }
    return @{ invoked_name = $Name; path = $absolute; before_sha256 = $before; after_sha256 = $after
        saved_sha256 = $saved; size = (Get-Item -LiteralPath $Destination).Length }
}
function Save-ReleaseAWindowsTools($Context) {
    $directory = Join-Path $Context.root 'toolchain/windows'
    New-Item -ItemType Directory -Path $directory | Out-Null
    $catalog = @{}
    foreach ($name in @('pwsh','python','wsl.exe','git','gpg.exe','curl.exe')) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        $path = if ($null -ne $command) { $command.Source } elseif ($name -ceq 'gpg.exe') { 'C:\Program Files\Git\usr\bin\gpg.exe' } else { throw "缺少Windows固定工具 $name" }
        $catalog[$name] = Save-ReleaseAExecutable $name $path (Join-Path $directory ($name + '.bin'))
    }
    Write-ReleaseAJson (Join-Path $directory 'catalog.json') $catalog
}
function Confirm-ReleaseAWindowsTools($Context) {
    $catalogPath = Join-Path $Context.root 'toolchain/windows/catalog.json'
    if (-not (Test-Path -LiteralPath $catalogPath)) { return }
    $catalog = Read-ReleaseAJson $catalogPath
    foreach ($name in $catalog.Keys) {
        $record = $catalog[$name]
        $record.final_sha256 = (Get-FileHash -LiteralPath $record.path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($record.final_sha256 -cne $record.before_sha256) { throw 'Windows固定工具在调用期间变化；拒绝身份' }
    }
    Write-ReleaseAJson $catalogPath $catalog
}
function Invoke-ReleaseARaw {
    param([string]$Exe, [string[]]$Arguments, [string]$Directory, [string]$RecordDirectory,
          [int]$TimeoutSeconds = 600, [scriptblock]$BeforeWindowsKill)
    New-Item -ItemType Directory -Path $RecordDirectory | Out-Null
    $record = @{ argv = @($Exe) + $Arguments; cwd = $Directory; exit = $null; timed_out = $false; error = $null; capture_complete = $false }
    Write-ReleaseAJson (Join-Path $RecordDirectory 'argv.json') @{ argv = $record.argv; cwd = $Directory }
    $process = [Diagnostics.Process]::new()
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.WorkingDirectory = $Directory
    $allowed = @('PATH','SystemRoot','WINDIR','TEMP','TMP','USERPROFILE','LOCALAPPDATA','APPDATA',
        'PROGRAMFILES','PROGRAMFILES(X86)','COMSPEC','PATHEXT','PSModulePath','NUMBER_OF_PROCESSORS','PROCESSOR_ARCHITECTURE')
    $info.Environment.Clear()
    foreach ($key in $allowed) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($null -ne $value) { $info.Environment[$key] = $value }
    }
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process.StartInfo = $info
    $out = $null; $err = $null; $outTask = $null; $errTask = $null; $started = $false; $cleanupDeadline = $null
    try {
        $out = [IO.File]::Open((Join-Path $RecordDirectory 'stdout.bin'), 'CreateNew', 'Write', 'Read')
        $err = [IO.File]::Open((Join-Path $RecordDirectory 'stderr.bin'), 'CreateNew', 'Write', 'Read')
        $resolved = (Get-Command $Exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $record.executable = Save-ReleaseAExecutable $Exe $resolved (Join-Path $RecordDirectory 'executable.bin')
        if (-not $process.Start()) { throw '固定child未启动' }
        $started = $true
        $outTask = $process.StandardOutput.BaseStream.CopyToAsync($out)
        $errTask = $process.StandardError.BaseStream.CopyToAsync($err)
        $record.windows_child = @{ pid = $process.Id; start_time_utc = $null; start_time_observed = $false }
        try { $record.windows_child.start_time_utc = $process.StartTime.ToUniversalTime().ToString('o'); $record.windows_child.start_time_observed = $true }
        catch { $record.windows_child.reason = $_.ToString() }
        $record.main_module = @{ observed = $false; reason = '快速进程已退出或MainModule不可观察' }
        try {
            $image = $process.MainModule.FileName
            if (-not [string]::IsNullOrWhiteSpace($image)) {
                $record.main_module = @{ observed = $true; path = $image }
            }
        }
        catch { $record.main_module.reason = $_.ToString() }
        # 双drain已先启动；可观察image另留原bytes，不阻塞任何一条原通道。
        if ($record.main_module.observed) {
            $record.main_module.image = Save-ReleaseAExecutable $Exe $record.main_module.path (Join-Path $RecordDirectory 'main-module.bin')
        }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $record.timed_out = $true
            if ($null -ne $BeforeWindowsKill) { & $BeforeWindowsKill }
            # Linux停止证明必须由callback完成；Windows kill自身不作该证明。
            $cleanupDeadline = [Environment]::TickCount64 + 10000
            if ($null -ne $BeforeWindowsKill -and -not $process.HasExited) {
                [void]$process.WaitForExit(2000) # Linux已确认停止，给当前Windows handle在同预算内交付原exit。
            }
            if (-not $process.HasExited) { $process.Kill($true) }
            $remainingCleanup = [int][Math]::Max(0, $cleanupDeadline - [Environment]::TickCount64)
            if (-not $process.WaitForExit($remainingCleanup)) { throw 'Windows child回收后仍未观察到退出' }
        }
        $record.exit = $process.ExitCode
        $drains = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($outTask, $errTask))
        if (-not $drains.Wait(15000)) { throw '双stream未在有界时间内到EOF' }
        $drains.GetAwaiter().GetResult()
        $out.Flush($true); $err.Flush($true)
    }
    catch {
        $record.error = $_.ToString()
        Write-ReleaseAJson (Join-Path $RecordDirectory 'error.json') @{ message = $_.ToString(); detail = $_.Exception.ToString() }
        # callback拒绝不能阻碍回收自己的Windows handle；它同时禁止稳定seal。
        if ($started -and -not $process.HasExited) {
            try {
                if ($null -eq $cleanupDeadline) { $cleanupDeadline = [Environment]::TickCount64 + 10000 }
                $process.Kill($true)
                $remainingCleanup = [int][Math]::Max(0, $cleanupDeadline - [Environment]::TickCount64)
                [void]$process.WaitForExit($remainingCleanup)
            }
            catch { $record.cleanup_error = $_.ToString() }
        }
    }
    finally {
        if ($started -and $process.HasExited) { $record.exit = $process.ExitCode }
        $record.terminated = $started -and $process.HasExited
        if ($null -ne $outTask -and $null -ne $errTask) {
            try {
                $drains = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($outTask, $errTask))
                if (-not $drains.Wait(15000)) { throw '失败收尾的原stream仍未到EOF' }
                $drains.GetAwaiter().GetResult()
                $out.Flush($true); $err.Flush($true)
                $record.capture_complete = $true
            }
            catch { if ($null -eq $record.error) { $record.error = $_.ToString() } }
        }
        if ($record.ContainsKey('executable')) {
            try {
                $record.executable.final_sha256 = (Get-FileHash -LiteralPath $record.executable.path -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($record.executable.final_sha256 -cne $record.executable.before_sha256) { throw 'Windows实际调用工具在前后变化' }
                if ($record.ContainsKey('main_module') -and $record.main_module.observed) {
                    $record.main_module.image.final_sha256 = (Get-FileHash -LiteralPath $record.main_module.image.path -Algorithm SHA256).Hash.ToLowerInvariant()
                    if ($record.main_module.image.final_sha256 -cne $record.main_module.image.before_sha256) { throw 'Windows实际MainModule在前后变化' }
                }
            }
            catch { if ($null -eq $record.error) { $record.error = $_.ToString() } }
        }
        if ($null -ne $out) { $out.Dispose() }
        if ($null -ne $err) { $err.Dispose() }
        Write-ReleaseAJson (Join-Path $RecordDirectory 'exit.json') $record
        $process.Dispose()
    }
    return $record
}
function Complete-ReleaseAPackReceipt([string]$ReceiptPath, [string]$RecordDirectory) {
    $receipt = Read-ReleaseAJson $ReceiptPath
    if ($null -ne $receipt.packing) { throw 'pack外部收据已封存；禁止覆盖原调用' }
    $record = Read-ReleaseAJson (Join-Path $RecordDirectory 'exit.json')
    Assert-ReleaseAResult $record '外部pack原记录'
    if (-not $record.terminated -or -not $record.capture_complete) { throw 'pack原streams/退出未完整，拒绝外部完成收据' }
    $packing = @{}
    foreach ($name in @('argv.json','stdout.bin','stderr.bin','exit.json','executable.bin','main-module.bin')) {
        $path = Join-Path $RecordDirectory $name
        if ($name -cin @('executable.bin','main-module.bin') -and -not (Test-Path -LiteralPath $path)) { continue }
        $bytes = [IO.File]::ReadAllBytes($path)
        $packing[$name] = @{ size = $bytes.Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
            base64 = [Convert]::ToBase64String($bytes) }
    }
    $receipt.packing = $packing
    Write-ReleaseAJson $ReceiptPath $receipt
}
function Get-ReleaseATransportBudget($Context) {
    $remaining = [int][Math]::Floor(($Context.transportDeadline - [DateTime]::UtcNow).TotalSeconds)
    if ($remaining -le 0) { throw 'pack与final-reader共用的10分钟transport预算耗尽；不再seal/重跑builder' }
    return [Math]::Min(600, $remaining)
}
function Get-ReleaseAFileIdentity([string]$Path) {
    return @{ size = (Get-Item -LiteralPath $Path).Length
        sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
}
function Invoke-ReleaseAFinalVerification($Context, $Paths) {
    $tar = Join-Path $env:RUNNER_TEMP 'release-a.tar'
    $receipt = Join-Path $env:RUNNER_TEMP 'release-a.receipt.json'
    $verification = Join-Path $env:RUNNER_TEMP 'release-a.verification.json'
    if (Test-Path -LiteralPath $verification) { throw '第三固定件已存在；禁止重验覆盖原记录' }
    $before = @{ tar = Get-ReleaseAFileIdentity $tar; receipt = Get-ReleaseAFileIdentity $receipt }
    $arguments = @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',
        "$($Paths.wsl)/tools/windows-h264-recipe-transport.py",'verify-pair',
        '--output',($Paths.evidence_wsl + '/../release-a.tar'),
        '--receipt',($Paths.evidence_wsl + '/../release-a.receipt.json'),
        '--expected-inputs',($Paths.evidence_wsl + '/checkout-inputs/blob-identities.json'),
        '--source',$env:GITHUB_SHA,'--run-id',$env:GITHUB_RUN_ID,'--run-attempt',$env:GITHUB_RUN_ATTEMPT)
    $directory = Join-Path $env:RUNNER_TEMP 'release-a-final-verify'
    $failure = $null; $record = $null; $after = $null
    try {
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $arguments -Directory $Context.repo `
            -RecordDirectory $directory -TimeoutSeconds (Get-ReleaseATransportBudget $Context)
        $after = @{ tar = Get-ReleaseAFileIdentity $tar; receipt = Get-ReleaseAFileIdentity $receipt }
        foreach ($name in @('tar','receipt')) {
            if ($before[$name].size -ne $after[$name].size -or $before[$name].sha256 -cne $after[$name].sha256) {
                throw '最终完整reader运行期间原两件变化；保留原件，不重seal'
            }
        }
        Assert-ReleaseAResult $record '最终packing pair完整reader'
        if (-not $record.capture_complete -or -not $record.terminated) { throw '最终reader原退出/EOF不完整' }
        $result = Read-ReleaseAJson (Join-Path $directory 'stdout.bin')
        $readerHash = (Get-FileHash -LiteralPath $Context.transport -Algorithm SHA256).Hash.ToLowerInvariant()
        $expectedPath = Join-Path $Context.root 'checkout-inputs/blob-identities.json'
        $expected = Read-ReleaseAJson $expectedPath
        if ($result.source -cne $env:GITHUB_SHA -or $result.run_id -cne $env:GITHUB_RUN_ID -or $result.run_attempt -cne $env:GITHUB_RUN_ATTEMPT -or
            $result.tar_size -ne $before.tar.size -or $result.tar_sha256 -cne $before.tar.sha256 -or
            $result.receipt_size -ne $before.receipt.size -or $result.receipt_sha256 -cne $before.receipt.sha256 -or
            $result.reader_sha256 -cne $readerHash -or $readerHash -cne $expected['tools/windows-h264-recipe-transport.py'] -or
            $result.expected_inputs_sha256 -cne (Get-FileHash -LiteralPath $expectedPath -Algorithm SHA256).Hash.ToLowerInvariant() -or
            -not $result.packing_complete -or $result.verification_complete -or $result.build_identity_passed) {
            throw '完整reader原stdout与独立source/run/reader/两输入身份不一致'
        }

    }
    catch { $failure = $_.ToString() }
    finally {
        $files = @{}
        foreach ($name in @('argv.json','stdout.bin','stderr.bin','exit.json','error.json','executable.bin','main-module.bin')) {
            $path = Join-Path $directory $name
            if (Test-Path -LiteralPath $path) {
                $bytes = [IO.File]::ReadAllBytes($path)
                $files[$name] = @{ size = $bytes.Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
                    base64 = [Convert]::ToBase64String($bytes) }
            }
        }
        Write-ReleaseAJson $verification @{ schema = 'freeremotedesk.windows.h264.release-a.verification.v1'
            source = $env:GITHUB_SHA; run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT
            reader_sha256 = (Get-FileHash -LiteralPath $Context.transport -Algorithm SHA256).Hash.ToLowerInvariant()
            inputs_before = $before; inputs_after = $after; files = $files; producer_error = $failure }
    }
    if ($null -ne $failure) { throw $failure }
}
function Complete-ReleaseATransport($Context, $Paths, [string]$ReceiptPath, [string]$PackRecordDirectory) {
    # 原两件在packing追加后即不可变；真实完整reader必须先于第三件写出。
    Complete-ReleaseAPackReceipt $ReceiptPath $PackRecordDirectory
    Invoke-ReleaseAFinalVerification $Context $Paths
}
function Assert-ReleaseAResult($Record, [string]$Name) {
    if ($null -ne $Record.error -or $Record.timed_out -or $Record.exit -ne 0) { throw "${Name}失败：$($Record | ConvertTo-Json -Compress)" }
}
function Get-ReleaseABootstrapError($Original) {
    $command=$null;$position=$null
    if($null -ne $Original.InvocationInfo){
        $position=$Original.InvocationInfo.PositionMessage
        if($null -ne $Original.InvocationInfo.MyCommand){$command=$Original.InvocationInfo.MyCommand.Name}
    }
    return @{ message=$Original.ToString(); exception=$Original.Exception.ToString()
        fully_qualified_error_id=$Original.FullyQualifiedErrorId; category=$Original.CategoryInfo.ToString()
        command=$command; position=$position
        script_stack_trace=$Original.ScriptStackTrace }
}
function Write-ReleaseABootstrapFirstError([string]$Path, $Original, [string]$Stage) {
    # 第二次入口/诊断保存失败不能覆盖第一次真实ErrorRecord；它不依赖state/Python/WSL。
    $value=@{ stage=$Stage; error=(Get-ReleaseABootstrapError $Original) }
    $bytes=[Text.UTF8Encoding]::new($false).GetBytes(($value | ConvertTo-Json -Depth 24) + "`n")
    $stream=[IO.File]::Open($Path,'CreateNew','Write','Read')
    try { $stream.Write($bytes,0,$bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
}
function Invoke-ReleaseABootstrapDiagnostic {
    param($Context, [string]$Source=$env:GITHUB_SHA, [string]$RunId=$env:GITHUB_RUN_ID,
        [string]$RunAttempt=$env:GITHUB_RUN_ATTEMPT)
    if (-not $IsWindows) { throw 'BootstrapDiagnostic只准实际Windows，不以Mac代替' }
    if ($Source -cnotmatch '^[0-9a-f]{40}$' -or $RunId -cnotmatch '^\d+$' -or $RunAttempt -cnotmatch '^\d+$') {
        throw 'bootstrap固定source/run身份格式错误'
    }
    $root=Join-Path $env:RUNNER_TEMP 'release-a-bootstrap-diagnostic'
    $summary=Join-Path $env:RUNNER_TEMP 'release-a-bootstrap-diagnostic.json'
    if ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $summary)) { throw 'bootstrap固定根已存在；禁止覆盖、清理后重试' }
    $result=@{ schema='freeremotedesk.windows.h264.bootstrap-diagnostic.v1'; source=$Source; run_id=$RunId; run_attempt=$RunAttempt
        mode='BootstrapDiagnostic'; formal_transport='incomplete-not-produced'; original_run_cause='UNKNOWN'
        original_failed_run=@{run_id='38847486035';run_attempt='1';source='ba6035752f746651681708c07cefa1c896248880'}
        forensic_only=$true; stable=$false; material_status='forensic-partial-never-stable'
        controls_passed=$false; first_failure=$null; diagnostic_errors=@(); cases=@{}; files=@{}; source_bindings=@{}
        pester=@{status='not-invoked';reason='必需controls不依赖Pester；另有受影响Pester测试供已安装环境使用'} }
    try {
        New-Item -ItemType Directory -Path $root | Out-Null
        $qualifier=Join-Path $Context.repo 'tools/qualify-windows-h264-recipe-A.ps1'
        $pwsh=(Get-Process -Id $PID).Path
        $bindings=$result.source_bindings
        foreach ($item in @(@{name='qualifier-source.ps1';path=$qualifier},@{name='transport-source.py';path=$Context.transport})) {
            $before=Get-ReleaseAFileIdentity $item.path
            [IO.File]::Copy($item.path,(Join-Path $root $item.name),$false)
            $copy=Get-ReleaseAFileIdentity (Join-Path $root $item.name); $after=Get-ReleaseAFileIdentity $item.path
            if ($before.size -ne $copy.size -or $before.sha256 -cne $copy.sha256 -or $before.sha256 -cne $after.sha256) {
                throw 'bootstrap源读取期间变化；不执行身份不定fixture'
            }
            $bindings[$item.name]=@{path=$item.path;before=$before;saved=$copy;after=$after}
        }
        $pythonCandidates=@(Get-Command 'python' -CommandType Application -ErrorAction SilentlyContinue)
        $pythonResolution=@{invoked_name='python';candidates=@($pythonCandidates | ForEach-Object {$_.Source})
            selected=$(if($pythonCandidates.Count -gt 0){$pythonCandidates[0].Source}else{$null})
            observation='host.json阶段的自然候选顺序；实际每次gateway独立解析，非该次进程启动证明'}
        Write-ReleaseAJson (Join-Path $root 'host.json') @{ source=$Source;run_id=$RunId;run_attempt=$RunAttempt
            os=$env:RUNNER_OS;arch=$env:RUNNER_ARCH;image=$env:ImageOS;image_version=$env:ImageVersion
            powershell=$PSVersionTable.PSVersion.ToString();process_id=$PID;pwsh=$pwsh;bindings=$bindings;python_resolution=$pythonResolution }
        $formatter=@'
param([string]$Qualifier,[string]$Results,[string]$Historical)
. $Qualifier
$tokens=$null;$errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($Qualifier,[ref]$tokens,[ref]$errors)
if ($errors.Count -ne 0) { throw '当前qualifier解析失败' }
$nodes=@($ast.FindAll({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -ceq 'Assert-ReleaseAResult'},$true))
if ($nodes.Count -ne 1) { throw 'formatter真实函数边界不唯一' }
$current=$nodes[0].Extent.Text
$fixed='${Name}失败：';$old='$Name失败：'
if ([regex]::Matches($current,[regex]::Escape($fixed)).Count -ne 1 -or $current.Contains($old)) { throw 'formatter唯一历史token边界错误' }
# 仅同一个真实函数的唯一token回退；函数体不镜像另写。局部scriptblock内装载原名，作用域退出即恢复新函数。
$history=$current.Replace($fixed,$old)
Write-ReleaseALf $Historical $history
$rows=@();$failed=$false
foreach ($sample in @(
    @{id='error';value=@{error='original-error-marker';timed_out=$false;exit=7}},
    @{id='exit';value=@{error=$null;timed_out=$false;exit=7}},
    @{id='timeout';value=@{error=$null;timed_out=$true;exit=0}})) {
    $red=& { param($Text,$Record) . ([scriptblock]::Create($Text)); try { Assert-ReleaseAResult $Record '初始化边界'; return $null } catch { return $_ } } $history $sample.value
    $green=$null
    try { Assert-ReleaseAResult $sample.value '初始化边界' } catch { $green=$_ }
    $redOk=$null -ne $red -and $red.FullyQualifiedErrorId -like '*VariableIsUndefined*' -and $red.ToString().Contains('Name失败')
    $greenOk=$null -ne $green -and $green.ToString().StartsWith('初始化边界失败：')
    if ($greenOk) {
        $decoded=$green.ToString().Substring('初始化边界失败：'.Length) | ConvertFrom-Json -AsHashtable
        $greenOk=$decoded.exit -eq $sample.value.exit -and $decoded.error -ceq $sample.value.error -and $decoded.timed_out -eq $sample.value.timed_out
    }
    $rows+=@{id=$sample.id;expected=$sample.value;historical_red=$redOk;current_green=$greenOk
        historical_error=$(if($null -ne $red){Get-ReleaseABootstrapError $red}else{$null})
        current_error=$(if($null -ne $green){Get-ReleaseABootstrapError $green}else{$null})}
    if (-not $redOk -or -not $greenOk) { $failed=$true }
}
try { Assert-ReleaseAResult @{error=$null;timed_out=$false;exit=0} '成功边界' } catch { $failed=$true }
$report=@{passed=(-not $failed);historical_token=$old;current_token=$fixed;cases=$rows}
Write-ReleaseAJson $Results $report
$report | ConvertTo-Json -Depth 24
if($failed){exit 1}
'@
        $binary=@'
$out=[Console]::OpenStandardOutput();$err=[Console]::OpenStandardError()
$a=[byte[]](0,255,65,10);$b=[byte[]](0,254,66,13)
for($i=0;$i -lt 16384;$i++){$out.Write($a,0,$a.Length);$err.Write($b,0,$b.Length)}
$out.Flush();$err.Flush();exit 7
'@
        foreach ($id in @('formatter','binary','missing-exe','log-conflict','initialize-success','initialize-failure')) {
            $caseRoot=Join-Path $root $id
            New-Item -ItemType Directory -Path $caseRoot | Out-Null
            $callRoot=Join-Path $caseRoot 'call';$exe=$pwsh;$arguments=@();$record=$null
            $case=@{passed=$false;raw_returned=$false;record=$null;outer_error=$null;intent=$null}
            $result.cases[$id]=$case
            try {
                switch ($id) {
                    'formatter' {
                        $script=Join-Path $caseRoot 'control.ps1';Write-ReleaseALf $script $formatter
                        $arguments=@('-NoProfile','-File',$script,$qualifier,(Join-Path $caseRoot 'results.json'),(Join-Path $caseRoot 'historical.ps1'))
                    }
                    'binary' {
                        $script=Join-Path $caseRoot 'control.ps1';Write-ReleaseALf $script $binary
                        $arguments=@('-NoProfile','-File',$script)
                    }
                    'missing-exe' { $exe=Join-Path $caseRoot 'fixed-never-created.exe';$arguments=@() }
                    'log-conflict' {
                        $callRoot=Join-Path $caseRoot 'record-directory-conflict.bin'
                        [IO.File]::WriteAllBytes($callRoot,[byte[]](0,255,11,22))
                        $arguments=@('-NoProfile','-Command','exit 0')
                    }
                    default {
                        $scratch=Join-Path $caseRoot 'root'
                        if ($id -ceq 'initialize-failure') {
                            New-Item -ItemType Directory -Path $scratch | Out-Null
                            [IO.File]::WriteAllBytes((Join-Path $caseRoot 'preexisting-root-marker.bin'),[byte[]](0,255,33,44))
                        }
                        $exe='python'
                        $arguments=@($Context.transport,'initialize','--root',$scratch,'--source',$Source,'--run-id',$RunId,'--run-attempt',$RunAttempt)
                    }
                }
                $case.intent=@{argv=@($exe)+$arguments;cwd=$Context.repo;record_directory=$callRoot}
                Write-ReleaseAJson (Join-Path $caseRoot 'intent.json') $case.intent
                $record=Invoke-ReleaseARaw -Exe $exe -Arguments $arguments -Directory $Context.repo -RecordDirectory $callRoot -TimeoutSeconds 30
                $case.raw_returned=$true;$case.record=$record
                if ($id -ceq 'missing-exe') {
                    if ($null -eq $record.error -or $null -ne $record.exit -or $record.terminated) { throw '不存在exe未保留真实启动失败/null exit' }
                    $expectedError=$null
                    try { Assert-ReleaseAResult $record '固定不存在exe' } catch { $expectedError=$_ }
                    if($null -eq $expectedError){throw '不存在exe被错误接受'}
                    $case.outer_error=Get-ReleaseABootstrapError $expectedError
                    try { Write-ReleaseABootstrapFirstError (Join-Path $caseRoot 'outer-error.json') $expectedError $id }
                    catch { $result.diagnostic_errors+=@(Get-ReleaseABootstrapError $_) }
                }
                else {
                    if ($null -ne $record.error -or $record.timed_out -or -not $record.terminated -or -not $record.capture_complete) { throw '实际child退出/原streams捕获不完整' }
                    if ($id -ceq 'binary') {
                        if ($record.exit -ne 7) { throw 'binary原exit不是7' }
                        $a=[IO.File]::ReadAllBytes((Join-Path $callRoot 'stdout.bin'));$b=[IO.File]::ReadAllBytes((Join-Path $callRoot 'stderr.bin'))
                        if ($a.Length -ne 65536 -or $b.Length -ne 65536) { throw 'binary原stream长度不符' }
                        for($i=0;$i -lt $a.Length;$i++) {
                            if ($a[$i] -ne ([byte[]](0,255,65,10))[$i%4] -or $b[$i] -ne ([byte[]](0,254,66,13))[$i%4]) { throw 'binary原stream逐byte不符' }
                        }
                    }
                    elseif ($id -ceq 'initialize-failure') {
                        if ($record.exit -eq 0 -or (Test-Path -LiteralPath (Join-Path $caseRoot 'root/provenance/state.json')) -or
                            (Get-Item -LiteralPath (Join-Path $callRoot 'stderr.bin')).Length -eq 0) { throw '预建root未产生真实非零/缺state/原stderr' }
                        $failureText=[Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $callRoot 'stderr.bin')))
                        if (-not $failureText.Contains('FileExistsError') -or @(Get-ChildItem -LiteralPath (Join-Path $caseRoot 'root') -Force).Count -ne 0) {
                            throw '实际initialize非零不是预建root的固定FileExistsError边界'
                        }
                        $expectedError=$null
                        try { Assert-ReleaseAResult $record '隔离initialize预建root' } catch { $expectedError=$_ }
                        if($null -eq $expectedError){throw '真实initialize非零被错误接受'}
                        $case.outer_error=Get-ReleaseABootstrapError $expectedError
                        try { Write-ReleaseABootstrapFirstError (Join-Path $caseRoot 'outer-error.json') $expectedError $id }
                        catch { $result.diagnostic_errors+=@(Get-ReleaseABootstrapError $_) }
                    }
                    else {
                        Assert-ReleaseAResult $record $id
                        if ($id -ceq 'formatter' -and -not (Read-ReleaseAJson (Join-Path $caseRoot 'results.json')).passed) { throw '实际formatter历史RED/当前GREEN未通过' }
                        if ($id -ceq 'initialize-success') {
                            $state=Read-ReleaseAJson (Join-Path $caseRoot 'root/provenance/state.json')
                            if ($state.source -cne $Source -or $state.run_id -cne $RunId -or $state.run_attempt -cne $RunAttempt -or
                                $state.stage -cne 'checkout' -or $state.checkout_ready -or $state.setup_ready -or $state.builder_started -or
                                $null -ne $state.attempt -or -not $state.quiescent) { throw 'initialize实际state身份/未启动后继阶段不符' }
                        }
                    }
                }
                $case.passed=$true
            }
            catch {
                $case.outer_error=Get-ReleaseABootstrapError $_
                try { Write-ReleaseABootstrapFirstError (Join-Path $caseRoot 'outer-error.json') $_ $id }
                catch { $result.diagnostic_errors+=@(Get-ReleaseABootstrapError $_) }
                # 只接受在实际gateway创建原日志目录之前的固定冲突；不把未知child失败当预期。
                if ($id -ceq 'log-conflict' -and $case.outer_error.command -ceq 'New-Item' -and
                    $case.outer_error.script_stack_trace -match 'Invoke-ReleaseARaw' -and -not $case.raw_returned -and $null -ne $case.intent -and
                    (Test-Path -LiteralPath $callRoot -PathType Leaf) -and
                    [Convert]::ToBase64String([IO.File]::ReadAllBytes($callRoot)) -ceq 'AP8LFg==') { $case.passed=$true }
            }
            if (-not $case.passed -and $null -eq $result.first_failure) {
                $result.first_failure=@{case=$id;error=$case.outer_error;record=$case.record}
            }
        }
        foreach($name in $bindings.Keys){
            $bindings[$name].final=Get-ReleaseAFileIdentity $bindings[$name].path
            if($bindings[$name].before.size -ne $bindings[$name].final.size -or $bindings[$name].before.sha256 -cne $bindings[$name].final.sha256){
                $result.diagnostic_errors+=@("bootstrap执行期间源变化：$name")
            }
        }
        $result.controls_passed=$null -eq $result.first_failure -and $result.cases.Count -eq 6 -and $result.diagnostic_errors.Count -eq 0
    }
    catch {
        if ($null -eq $result.first_failure) { $result.first_failure=@{case='diagnostic-entry';error=(Get-ReleaseABootstrapError $_)} }
        else { $result.diagnostic_errors+=@(Get-ReleaseABootstrapError $_) }
    }
    finally {
        # 固定allowlist只是forensic观察，不是POSIX seal；任何未知EOF/存活/缺件都不升stable。
        $fixed=@('host.json','qualifier-source.ps1','transport-source.py','formatter/control.ps1','formatter/historical.ps1','formatter/results.json',
            'binary/control.ps1','log-conflict/record-directory-conflict.bin','initialize-success/root/provenance/state.json','initialize-failure/preexisting-root-marker.bin')
        foreach($id in @('formatter','binary','missing-exe','log-conflict','initialize-success','initialize-failure')) {
            $fixed+=@("$id/intent.json","$id/outer-error.json")
            foreach($name in @('argv.json','stdout.bin','stderr.bin','exit.json','error.json','executable.bin','main-module.bin')) { $fixed+=@("$id/call/$name") }
        }
        foreach($relative in $fixed) {
            $entry=@{present=$false;stable=$false;status='missing-partial';writer_status='not-observed';before=$null;after=$null;error=$null}
            $result.files[$relative]=$entry
            $path=Join-Path $root $relative
            try {
                if(Test-Path -LiteralPath $path -PathType Leaf) {
                    $entry.present=$true;$entry.before=Get-ReleaseAFileIdentity $path;$entry.after=Get-ReleaseAFileIdentity $path
                    $entry.writer_status='owned-diagnostic-file-observed'
                    $caseId=$relative.Split('/')[0]
                    if($result.cases.ContainsKey($caseId)){
                        $owner=$result.cases[$caseId]
                        $entry.writer_status=if($owner.raw_returned -and $null -ne $owner.record -and $owner.record.terminated -and $owner.record.capture_complete){'raw-child-exit-and-eof-observed'}else{'writer-or-eof-unconfirmed'}
                    }
                    $entry.status=if($entry.before.size -ne $entry.after.size -or $entry.before.sha256 -cne $entry.after.sha256){'changed-partial'}
                        elseif($entry.writer_status -ceq 'writer-or-eof-unconfirmed'){'writer-or-eof-unconfirmed-partial'}else{'forensic-observed-not-stable'}
                    if($entry.status -ceq 'changed-partial'){$result.diagnostic_errors+=@("读取期间变化：$relative")}
                }
            }
            catch {$entry.status='unreadable-partial';$entry.error=Get-ReleaseABootstrapError $_;$result.diagnostic_errors+=@($entry.error)}
        }
        if($result.diagnostic_errors.Count -ne 0){$result.controls_passed=$false}
        Write-ReleaseAJson $summary $result
    }
    if(-not $result.controls_passed){throw 'BootstrapDiagnostic必需controls失败；保留原first_failure与独立diagnostic_errors，不升级自然builder'}
    return $result
}
function Quote-ReleaseABash([string]$Value) {
    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $singleQuoteEscape) + "'"
}
function Get-ReleaseASetupBudget($Context, [int]$Cap = 600) {
    $remaining = [int][Math]::Floor(($Context.setupDeadline - [DateTime]::UtcNow).TotalSeconds)
    if ($remaining -le 0) { throw 'setup总预算10分钟耗尽；不得挤占60分钟builder及收尾预算' }
    return [Math]::Min($Cap, $remaining)
}
function Get-ReleaseAWslPath($Context, [string]$Path, [string]$Id, [switch]$Reverse) {
    $flag = if ($Reverse) { '-w' } else { '-a' }
    $result = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--distribution','Ubuntu-24.04','--user','root',
        '--exec','wslpath',$flag,$Path) -Directory $Context.repo -RecordDirectory (Join-Path $Context.root "setup/calls/$Id") -TimeoutSeconds (Get-ReleaseASetupBudget $Context 30)
    Assert-ReleaseAResult $result 'wslpath'
    return [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $Context.root "setup/calls/$Id/stdout.bin"))).Trim()
}
function Stop-ReleaseAScopes($Context, [switch]$ControlsOnly, [switch]$SetupOnly) {
    # 只执行本checkout提取且已核对bytes的同job脚本，永不从下载artifact取可执行程序。
    $expected = (Get-ReleaseASupervisorSource $Context.repo).Replace("`r`n","`n").TrimEnd([char[]]"`r`n") + "`n"
    $scopes = @()
    $kinds = if ($SetupOnly) { @('setup') } elseif ($ControlsOnly) { @('control-normal','control-halt') } else { @('setup','collect','control-normal','control-halt') }
    foreach ($kind in $kinds) {
        $intent = Join-Path $Context.root "setup/$kind-intent.json"
        if (Test-Path -LiteralPath $intent) { $scopes += @{ kind = $kind; intent = $intent
            source = (Join-Path $Context.root 'setup/lifecycle-supervisor.py') } }
    }
    if (-not $ControlsOnly -and -not $SetupOnly) {
    $configuration = Join-Path $Context.repo '.codex-target/ffmpeg-8.1.2/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
    New-Item -ItemType Directory -Force -Path $configuration | Out-Null
    [IO.File]::WriteAllText((Join-Path $configuration 'lifecycle-halt'), 'no new Linux launch')
    $attempts = @(Get-ChildItem -LiteralPath $configuration -Force -Directory -Filter '.attempt-*')
    if ($attempts.Count -gt 1) { throw '多个attempt；禁止猜选最新材料' }
    foreach ($attempt in $attempts) {
        [IO.File]::WriteAllText((Join-Path $attempt.FullName 'lifecycle-halt'), 'no new Linux launch')
        $intent = Join-Path $attempt.FullName 'lifecycle-intent.json'
        if (Test-Path -LiteralPath $intent) { $scopes += @{ kind = 'builder'; intent = $intent
            source = (Join-Path $attempt.FullName 'release-a-supervisor.py') } }
    }
    }
    foreach ($item in $scopes) {
        $intent = Read-ReleaseAJson $item.intent
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($item.source)) -cne
            [Convert]::ToBase64String([Text.UTF8Encoding]::new($false).GetBytes($expected))) { throw '当前job supervisor bytes不匹配' }
        if ((Get-FileHash -LiteralPath $item.source -Algorithm SHA256).Hash.ToLowerInvariant() -cne $intent.source_sha256) {
            throw 'scope与当前生成source不匹配'
        }
        $callRoot = Join-Path $Context.root ("setup/calls/linux-cleanup-" + $item.kind)
        if (Test-Path -LiteralPath $callRoot) {
            $prior = Read-ReleaseAJson (Join-Path $callRoot 'exit.json')
            Assert-ReleaseAResult $prior '前次cleanup不成功，禁止重试掩盖'
            continue
        }
        $commands = @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',$intent.supervisor_linux,
            '--scope',$intent.scope_linux,'--nonce',$intent.nonce,'stop')
        $result = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $commands -Directory $Context.repo -RecordDirectory $callRoot -TimeoutSeconds 20
        Assert-ReleaseAResult $result 'Linux scope停止证明'
    }
    Write-ReleaseAJson (Join-Path $Context.root 'setup/lifecycle.json') @{ stable = $true; scopes = $scopes; no_launches_after_halt = $true }
}

function Invoke-ReleaseALinuxControls($Context) {
    $worker = @'
#!/usr/bin/env python3
"""固定Linux-only nobody/同组控制；没有账户/apt变更，无任意worker命令。"""
import argparse
import json
import os
from pathlib import Path
import pwd
import select
import signal
import sys
import time


def main():
    if sys.platform != 'linux' or os.geteuid() != 0:
        raise RuntimeError('控制只支持真实Linux root；不能用Mac冒充')
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=('normal', 'halt'))
    parser.add_argument('--scope', type=Path, required=True)
    parser.add_argument('--call-id', required=True)
    parser.add_argument('--nonce', required=True)
    args = parser.parse_args()
    account = pwd.getpwnam('nobody')  # 缺失明确失败，绝不换另一个服务账号。
    if account.pw_uid == 0:
        raise RuntimeError('nobody不是非root账户；setup不支持')
    report_r, report_w = os.pipe()
    go_r, go_w = os.pipe()
    child = os.fork()
    if child == 0:
        try:
            os.close(report_r); os.close(go_w)
            os.setgroups([]); os.setgid(account.pw_gid); os.setuid(account.pw_uid)
            report = dict(pid=os.getpid(), uid=os.getuid(), euid=os.geteuid(), gid=os.getgid(),
                          sid=os.getsid(0), pgid=os.getpgrp(), uid_values=list(os.getresuid()))
            os.write(report_w, (json.dumps(report) + '\n').encode()); os.close(report_w)
            if not select.select([go_r], [], [], 7)[0] or os.read(go_r, 1) != b'G':
                os._exit(126)
            os.close(go_r)
            if args.mode == 'halt':
                signal.alarm(8)
                signal.pause()
            os._exit(0)
        except BaseException as error:
            os.write(2, str(error).encode()); os._exit(126)
    os.close(report_w); os.close(go_r)
    if not select.select([report_r], [], [], 5)[0]:
        raise RuntimeError('降权child没有pipe UID报告')
    with os.fdopen(report_r, 'rb') as report_stream:
        report = json.loads(report_stream.readline())
    if report['pid'] != child or report['uid'] != account.pw_uid or report['euid'] != account.pw_uid or report['gid'] != account.pw_gid:
        raise RuntimeError('实际pipe UID/GID不匹配passwd fixture')
    print(json.dumps(dict(schema='freeremotedesk.release-a.linux-control.v1', mode=args.mode,
        fixture_account='nobody', passwd_uid=account.pw_uid, passwd_gid=account.pw_gid, report=report)), flush=True)
    observed = args.scope / (args.call_id + '.observations.json')
    deadline = time.monotonic() + 5
    while not observed.exists():
        if time.monotonic() > deadline:
            raise RuntimeError('原supervisor尚未实际观察owned UID差异')
        time.sleep(.02)
    observation = json.loads(observed.read_bytes())
    if observation['nonce'] != args.nonce or observation['call_id'] != args.call_id or not any(
            item['pid'] == child and item['uid'] == account.pw_uid and item['uid_values'][1] == account.pw_uid
            for item in observation['owned_uid_differences']):
        raise RuntimeError('UID观察不属于本次真实child')
    os.write(go_w, b'G'); os.close(go_w)
    _, status = os.waitpid(child, 0)
    if args.mode != 'normal' or os.waitstatus_to_exitcode(status) != 0:
        raise RuntimeError('控制child提前退出；HALT未取得预期原终态')
    return 0


if __name__ == '__main__':
    sys.exit(main())

'@
    Write-ReleaseALf (Join-Path $Context.root 'setup/linux-control-worker.py') $worker
    $results = @{}
    foreach ($mode in @('normal','halt')) {
        $kind = 'control-' + $mode
        $nonce = [Guid]::NewGuid().ToString('N'); $callId = [Guid]::NewGuid().ToString('N')
        $scope = "$($Context.wslRoot)/setup/groups/$kind"
        New-Item -ItemType Directory -Path (Join-Path $Context.root "setup/groups/$kind") | Out-Null
        Write-ReleaseAJson (Join-Path $Context.root "setup/$kind-intent.json") @{ nonce = $nonce
            supervisor_linux = "$($Context.wslRoot)/setup/lifecycle-supervisor.py"; scope_linux = $scope
            source_sha256 = (Get-FileHash -LiteralPath (Join-Path $Context.root 'setup/lifecycle-supervisor.py') -Algorithm SHA256).Hash.ToLowerInvariant() }
        $command = "exec python3 $(Quote-ReleaseABash ($Context.wslRoot + '/setup/linux-control-worker.py')) $mode --scope $(Quote-ReleaseABash $scope) --call-id $callId --nonce $nonce"
        $arguments = @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',
            "$($Context.wslRoot)/setup/lifecycle-supervisor.py",'--scope',$scope,'--nonce',$nonce,'--call-id',$callId,
            'run','--','bash','-lc',$command)
        $budget = Get-ReleaseASetupBudget $Context $(if ($mode -ceq 'halt') { 4 } else { 8 })
        if ($mode -ceq 'halt') {
            $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $arguments -Directory $Context.repo `
                -RecordDirectory (Join-Path $Context.root "setup/calls/linux-control-$mode") -TimeoutSeconds $budget `
                -BeforeWindowsKill { Stop-ReleaseAScopes $Context -ControlsOnly }
            if (-not $record.timed_out -or $record.exit -ne 124 -or $null -ne $record.error -or -not $record.capture_complete -or -not $record.terminated) {
                throw 'Linux HALT控制没有取得预期原exit124/EOF；不进入自然builder'
            }
        }
        else {
            $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $arguments -Directory $Context.repo `
                -RecordDirectory (Join-Path $Context.root "setup/calls/linux-control-$mode") -TimeoutSeconds $budget `
                -BeforeWindowsKill { Stop-ReleaseAScopes $Context -ControlsOnly }
            Assert-ReleaseAResult $record 'Linux正常降权控制'
        }
        $scopeWindows = Join-Path $Context.root "setup/groups/$kind"
        $end = Read-ReleaseAJson (Join-Path $scopeWindows "$callId.end.json")
        $observation = Read-ReleaseAJson (Join-Path $scopeWindows "$callId.observations.json")
        if ($end.nonce -cne $nonce -or -not $end.quiescent -or -not $end.reaped -or -not $end.observed_uid_difference -or
            $observation.nonce -cne $nonce -or $observation.call_id -cne $callId) { throw 'Linux控制租约/真实UID观察/终态未闭合' }
        $results[$mode] = @{ nonce = $nonce; call_id = $callId; actual_exit = $record.exit
            actual_timed_out = $record.timed_out; expected_exit = $(if ($mode -ceq 'halt') { 124 } else { 0 }) }
    }
    Write-ReleaseAJson (Join-Path $Context.root 'setup/control-results.json') @{ schema = 'freeremotedesk.release-a.linux-controls.v1'; results = $results }
}

function Invoke-ReleaseABuilderOnce($Context, [scriptblock]$Runner) {
    if ($Context.builderInvocations -ne 0) { throw 'builder只能调用一次' }
    $Context.builderInvocations = 1
    $arguments = @('-NoProfile','-File',(Join-Path $Context.repo 'tools/build-ffmpeg-windows.ps1'),
        '-Configuration','Release','-Architecture','arm64','-WslDistribution','Ubuntu-24.04','-SourceRecipe','frd-h264dc-aarch64-v1')
    return & $Runner 'pwsh' $arguments
}
function Get-ReleaseAContext {
    $repo = [IO.Path]::GetFullPath($env:GITHUB_WORKSPACE)
    $root = Join-Path $env:RUNNER_TEMP 'frd-release-a-evidence'
    return @{ repo = $repo; root = $root; transport = Join-Path $repo 'tools/windows-h264-recipe-transport.py'; builderInvocations = 0 }
}
function Invoke-ReleaseAAptIdentity($Context, $State) {
    # 只读固定探测；原apt安装仍由原supervisor内的prepare前缀执行。
    $aptExecutables = @'
import hashlib,json,shutil,sys
from pathlib import Path


def executable_identity(path):
    path = Path(path)
    before = path.stat()
    first = path.read_bytes()
    second = path.read_bytes()
    after = path.stat()
    stable = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) == (
        after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns) and first == second
    return dict(path=str(path), resolved=str(path.resolve()), size=len(first),
                before_sha256=hashlib.sha256(first).hexdigest(), after_sha256=hashlib.sha256(second).hexdigest(),
                observed_read_consistent=stable)


def main():
    if sys.platform != 'linux':
        raise RuntimeError('APT身份探测只在真实Linux执行')
    tools = {}
    for name in ('apt', 'apt-get', 'dpkg', 'apt-config'):
        path = shutil.which(name)
        if path is None:
            raise RuntimeError('缺少固定APT工具: ' + name)
        tools[name] = executable_identity(path)
    print(json.dumps(dict(schema='freeremotedesk.release-a.apt-executables.v1', tools=tools), sort_keys=True))
    if not all(item['observed_read_consistent'] for item in tools.values()):
        raise RuntimeError('APT executable读取期间变化；不进入apt前缀')


if __name__ == '__main__':
    main()
'@
    Write-ReleaseALf (Join-Path $Context.root 'setup/apt-executables.py') $aptExecutables
    $probes = @(
        @{ id = 'apt-version'; argv = @('apt','--version') },
        @{ id = 'apt-get-version'; argv = @('apt-get','--version') },
        @{ id = 'dpkg-version'; argv = @('dpkg','--version') },
        # 空stdout表示没有显式值，不能冒充已证明默认true/false；不dump全部配置。
        @{ id = 'apt-use-pty'; argv = @('apt-config','shell','FRD_DPKG_USE_PTY','Dpkg::Use-Pty') },
        @{ id = 'apt-executables'; argv = @('python3',"$($Context.wslRoot)/setup/apt-executables.py") }
    )
    $firstProbeFailure = $null
    foreach ($probe in $probes) {
        try {
            $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments (@('--distribution','Ubuntu-24.04','--user','root','--exec') + $probe.argv) `
                -Directory $Context.repo -RecordDirectory (Join-Path $Context.root ("setup/calls/" + $probe.id)) -TimeoutSeconds (Get-ReleaseASetupBudget $Context 30)
            Assert-ReleaseAResult $record ("APT只读身份探测 " + $probe.id)
        }
        catch {
            Add-ReleaseAError $State 'apt_diagnostic_probe' ($probe.id + ': ' + $_.ToString())
            if ($null -eq $firstProbeFailure) { $firstProbeFailure = $_ }
        }
    }
    if ($null -ne $firstProbeFailure) { throw $firstProbeFailure }
}
function Save-ReleaseAAptReceipts($Context) {
    $callId = $Context.setupCallId
    if ($callId -cnotmatch '^[a-f0-9]{32}$') { throw '诊断callid非法，禁止读取任意路径' }
    $files = @{}
    $complete = $true
    foreach ($kind in @('pending','start','end','observations')) {
        $name = "$callId.$kind.json"
        $path = Join-Path $Context.root ("setup/groups/setup/" + $name)
        $entry = @{ original_windows_path = $path; original_linux_path = "$($Context.wslRoot)/setup/groups/setup/$name"
            optional = ($kind -ceq 'observations'); status = 'missing'; before = $null; after = $null }
        try {
            if (Test-Path -LiteralPath $path) {
                $before = Get-Item -LiteralPath $path -Force
                if ($before.PSIsContainer -or ($before.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'UUID原文件不是普通文件' }
                $entry.metadata_before = @{ size = $before.Length; mtime_ticks = $before.LastWriteTimeUtc.Ticks }
                $first = [IO.File]::ReadAllBytes($path)
                $entry.before = @{ bytes_base64 = [Convert]::ToBase64String($first)
                    sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($first)).ToLowerInvariant(); size = $first.Length }
                $second = [IO.File]::ReadAllBytes($path)
                $entry.after = @{ bytes_base64 = [Convert]::ToBase64String($second)
                    sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($second)).ToLowerInvariant(); size = $second.Length }
                $after = Get-Item -LiteralPath $path -Force
                $entry.metadata_after = @{ size = $after.Length; mtime_ticks = $after.LastWriteTimeUtc.Ticks }
                $entry.status = 'changed-partial'
                if ($entry.metadata_before.size -eq $entry.metadata_after.size -and $entry.metadata_before.mtime_ticks -eq $entry.metadata_after.mtime_ticks -and
                    -not ($after.Attributes -band [IO.FileAttributes]::ReparsePoint) -and
                    $entry.before.bytes_base64 -ceq $entry.after.bytes_base64) {
                    $entry.status = 'observed-byte-identical'
                    $value = [Text.Encoding]::UTF8.GetString($first) | ConvertFrom-Json -AsHashtable
                    $entry.binding_matches = ($value.nonce -ceq $Context.setupNonce -and $value.call_id -ceq $callId)
                    if (-not $entry.binding_matches) { $entry.status = 'binding-mismatch-partial' }
                }
            }
        }
        catch { $entry.status = 'read-error-partial'; $entry.error = Get-ReleaseABootstrapError $_ }
        if ($entry.status -cne 'observed-byte-identical' -and -not ($entry.optional -and $entry.status -ceq 'missing')) { $complete = $false }
        $files[$kind] = $entry
    }
    Write-ReleaseAJson (Join-Path $Context.root 'setup/apt-diagnostic-receipts.json') @{
        schema = 'freeremotedesk.release-a.apt-receipts.v1'; nonce = $Context.setupNonce; call_id = $callId
        read_boundary = 'two byte-identical reads plus size/mtime observation only; not a no-future-write or quiescence proof'
        source_sha256 = (Get-FileHash -LiteralPath (Join-Path $Context.root 'setup/lifecycle-supervisor.py') -Algorithm SHA256).Hash.ToLowerInvariant()
        complete_observation = $complete; files = $files }
    if (-not $complete) { throw '本callid UUID原材料读取不完整；已保留partial receipt，不升级诊断成功' }
}
function Invoke-ReleaseAExecute($Context) {
    $aptDiagnostic = $Context.ContainsKey('bootstrapAptDiagnostic') -and $Context.bootstrapAptDiagnostic
    $Context.setupDeadline = [DateTime]::UtcNow.AddMinutes(10)
    $statePath = Join-Path $Context.root 'provenance/state.json'
    $initialLog = Join-Path $env:RUNNER_TEMP 'release-a-initialize'
    $record = Invoke-ReleaseARaw -Exe 'python' -Arguments @($Context.transport,'initialize','--root',$Context.root,
        '--source',$env:GITHUB_SHA,'--run-id',$env:GITHUB_RUN_ID,'--run-attempt',$env:GITHUB_RUN_ATTEMPT) `
        -Directory $Context.repo -RecordDirectory $initialLog -TimeoutSeconds 30
    Assert-ReleaseAResult $record '运输root初始化'
    New-Item -ItemType Directory -Path (Join-Path $Context.root 'setup/calls') | Out-Null
    Move-Item -LiteralPath $initialLog -Destination (Join-Path $Context.root 'setup/calls/initialize')
    $state = Read-ReleaseAJson $statePath
    try {
        # 仅此独立checkout的info/attributes与index；不写全局或仓库attributes。
        $record = Invoke-ReleaseARaw -Exe 'python' -Arguments @($Context.transport,'checkout','--repo',$Context.repo,
            '--source',$env:GITHUB_SHA,'--output',(Join-Path $Context.root 'checkout-inputs')) -Directory $Context.repo `
            -RecordDirectory (Join-Path $Context.root 'setup/calls/checkout') -TimeoutSeconds (Get-ReleaseASetupBudget $Context 120)
        Assert-ReleaseAResult $record '固定blob checkout'
        Write-ReleaseAJson (Join-Path $Context.root 'provenance/checkout.json') @{ source = $env:GITHUB_SHA; workflow_ref = $env:RELEASE_A_WORKFLOW_REF
            workflow_sha = $env:RELEASE_A_WORKFLOW_SHA; repository = $env:GITHUB_REPOSITORY; ref = $env:GITHUB_REF; event = $env:GITHUB_EVENT_NAME }
        Write-ReleaseAJson (Join-Path $Context.root 'setup/host.json') @{ os = $env:RUNNER_OS; arch = $env:RUNNER_ARCH
            image = $env:ImageOS; image_version = $env:ImageVersion; powershell = $PSVersionTable.PSVersion.ToString() }
        $state.checkout_ready = $true; $state.stage = 'setup'
        Save-ReleaseAWindowsTools $Context
        Write-ReleaseAJson $statePath $state
        $fresh = Join-Path $Context.repo '.codex-target/ffmpeg-8.1.2'
        if (Test-Path -LiteralPath $fresh) { throw 'A配置根已存在；禁止复用、清理后重试或选旧attempt' }
        if ($Context.ContainsKey('bootstrapCheckoutDiagnostic') -and $Context.bootstrapCheckoutDiagnostic) { return }
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--install','--distribution','Ubuntu-24.04','--no-launch') `
            -Directory $Context.repo -RecordDirectory (Join-Path $Context.root 'setup/calls/wsl-install') -TimeoutSeconds (Get-ReleaseASetupBudget $Context 300)
        Assert-ReleaseAResult $record 'WSL安装（非0/需重启均STOP）'
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--version') -Directory $Context.repo `
            -RecordDirectory (Join-Path $Context.root 'setup/calls/wsl-version') -TimeoutSeconds (Get-ReleaseASetupBudget $Context 30)
        Assert-ReleaseAResult $record 'WSL版本'
        $Context.wslRoot = Get-ReleaseAWslPath $Context $Context.root 'path-to-wsl'
        $roundtrip = Get-ReleaseAWslPath $Context $Context.wslRoot 'path-to-windows' -Reverse
        if ([IO.Path]::GetFullPath($roundtrip) -cne [IO.Path]::GetFullPath($Context.root)) { throw 'WSL路径往返不匹配' }
        # repo与evidence为RUNNER_TEMP不同位置；由同一次真实wslpath额外转换固定repo。
        $repoPathRecord = Join-Path $Context.root 'setup/calls/path-repo'
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--distribution','Ubuntu-24.04','--user','root','--exec','wslpath','-a',$Context.repo) `
            -Directory $Context.repo -RecordDirectory $repoPathRecord -TimeoutSeconds (Get-ReleaseASetupBudget $Context 30)
        Assert-ReleaseAResult $record 'repo wslpath'
        $Context.wslRepo = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $repoPathRecord 'stdout.bin'))).Trim()
        $repoRoundtrip = Get-ReleaseAWslPath $Context $Context.wslRepo 'path-repo-reverse' -Reverse
        if ([IO.Path]::GetFullPath($repoRoundtrip) -cne [IO.Path]::GetFullPath($Context.repo)) { throw 'repo WSL路径往返不匹配' }
        $state.build_origin = $Context.wslRepo + '/.codex-target/ffmpeg-8.1.2'
        Write-ReleaseAJson (Join-Path $Context.root 'provenance/paths.json') @{ windows = $Context.repo; wsl = $Context.wslRepo
            evidence_windows = $Context.root; evidence_wsl = $Context.wslRoot; roundtrip = $roundtrip; repo_roundtrip = $repoRoundtrip }
        Write-ReleaseALf (Join-Path $Context.root 'setup/lifecycle-supervisor.py') (Get-ReleaseASupervisorSource $Context.repo)
        $Context.setupNonce = [Guid]::NewGuid().ToString('N')
        New-Item -ItemType Directory -Force -Path (Join-Path $Context.root 'setup/groups/setup') | Out-Null
        Write-ReleaseAJson (Join-Path $Context.root 'setup/setup-intent.json') @{ nonce = $Context.setupNonce
            supervisor_linux = "$($Context.wslRoot)/setup/lifecycle-supervisor.py"; scope_linux = "$($Context.wslRoot)/setup/groups/setup"
            source_sha256 = (Get-FileHash -LiteralPath (Join-Path $Context.root 'setup/lifecycle-supervisor.py') -Algorithm SHA256).Hash.ToLowerInvariant() }
        $prepare = @'
#!/bin/bash
set -euo pipefail
root=$1
. /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == 24.04 ]]
cp /etc/os-release "$root/setup/os-release"
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get -o Dpkg::Use-Pty=0 install --no-install-recommends -y python3 build-essential make patch pkg-config xz-utils tar curl ca-certificates procps util-linux
python3 -c 'import sys; assert sys.version_info[:2] == (3,12)'
dpkg-query -W > "$root/setup/apt-packages.txt"
[[ ! -e /opt/frd-llvm-mingw-20250709 ]]
curl --fail --location --proto '=https' --tlsv1.2 https://github.com/mstorsjo/llvm-mingw/releases/download/20250709/llvm-mingw-20250709-msvcrt-ubuntu-22.04-x86_64.tar.xz -o "$root/toolchain/archive.tar.xz.partial"
printf '%s  %s\n' 422f13915a41acd8b93566e590531f55d3de6e531575197d51756ff8e8d16525 "$root/toolchain/archive.tar.xz.partial" | sha256sum --check --strict
mv "$root/toolchain/archive.tar.xz.partial" "$root/toolchain/archive.tar.xz"
mkdir /opt/frd-llvm-mingw-20250709
tar -xJf "$root/toolchain/archive.tar.xz" --strip-components=1 -C /opt/frd-llvm-mingw-20250709
python3 - "$root" <<'PYCODE'
import hashlib,json,os,shutil,sys
from pathlib import Path
names=('python3','env','make','patch','bash','tar','xz','curl')
result={'path':os.environ.get('PATH'),'python':sys.version,'kernel':os.uname().release,'tools':{}}
for name in names:
 p=Path(shutil.which(name));result['tools'][name]={'path':str(p),'resolved':str(p.resolve()),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
Path(sys.argv[1]+'/setup/tools.json').write_text(json.dumps(result,indent=2)+'\n')
PYCODE
/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang --version
/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-strip --version
'@
        if ($aptDiagnostic) {
            # 使用同一原prepare源，仅诊断截在原apt install之后；普通Execute不走此分支。
            $boundary = "`npython3 -c 'import sys; assert sys.version_info[:2] == (3,12)'"
            $boundaryAt = $prepare.IndexOf($boundary, [StringComparison]::Ordinal)
            if ($boundaryAt -lt 0 -or $prepare.IndexOf($boundary, $boundaryAt + 1, [StringComparison]::Ordinal) -ge 0) { throw '原apt前缀截断边界不唯一' }
            $prepare = $prepare.Substring(0, $boundaryAt)
        }
        $collect = @'
#!/bin/bash
set -euo pipefail
root=$1
repo=$2
mark_phase() {
python3 - "$root/../release-a-collect-phases.json" "$1" "$2" "${3-}" <<'FRD_PHASE'
# FRD_PHASE_BEGIN
import json,os,stat,sys,time
from pathlib import Path


def phase_update(path, phase, event, code=None):
    names = ('llvm-copy','system-tools','build-root-copy')
    if phase not in names or event not in ('start','end'):
        raise ValueError('fixed phase only')
    path = Path(path)
    value = dict(schema='freeremotedesk.release-a.collect-phases.v1', phases={name: None for name in names})
    if os.path.lexists(path):
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or getattr(info, 'st_file_attributes', 0) & 0x400 or info.st_size > 4096:
            raise ValueError('phase file type/size')
        value = json.loads(path.read_bytes())
    if set(value) != {'schema','phases'} or value['schema'] != 'freeremotedesk.release-a.collect-phases.v1' or set(value['phases']) != set(names):
        raise ValueError('phase schema')
    records = value['phases']; position = names.index(phase)
    if any(records[name] is None or records[name]['exit'] != 0 for name in names[:position]) or any(records[name] is not None for name in names[position+1:]):
        raise ValueError('phase order')
    if event == 'start':
        if records[phase] is not None or code is not None: raise ValueError('duplicate phase start')
        records[phase] = dict(start_ns=time.time_ns(), end_ns=None, exit=None)
    else:
        if records[phase] is None or records[phase]['end_ns'] is not None or type(code) is not int or not 0 <= code <= 255:
            raise ValueError('phase exit')
        records[phase].update(end_ns=time.time_ns(), exit=code)
    raw = (json.dumps(value,sort_keys=True,indent=2)+'\n').encode()
    if len(raw) > 4096: raise ValueError('phase byte bound')
    temporary = path.with_name(path.name+'.writing')
    with temporary.open('xb') as output:
        output.write(raw); output.flush(); os.fsync(output.fileno())
    temporary.replace(path)
    return value


if __name__ == '__main__':
    phase_update(sys.argv[1], sys.argv[2], sys.argv[3], None if not sys.argv[4] else int(sys.argv[4]))
# FRD_PHASE_END
FRD_PHASE
}
finish_phase() {
    local bulk_exit=$2 marker_exit=0
    if mark_phase "$1" end "$bulk_exit"; then
        :
    else
        marker_exit=$?
        printf 'phase marker失败: %s (exit=%s)\n' "$1" "$marker_exit" >&2 || :
    fi
    # marker失败只追加原stderr；原bulk非零优先返回。
    [[ $bulk_exit -eq 0 ]] || exit "$bulk_exit"
    [[ $marker_exit -eq 0 ]] || exit "$marker_exit"
}
mark_phase llvm-copy start
set +e
if [[ -e /opt/frd-llvm-mingw-20250709 ]]; then cp -a /opt/frd-llvm-mingw-20250709 "$root/toolchain/install"; fi
phase_exit=$?
set -e
finish_phase llvm-copy "$phase_exit"
mark_phase system-tools start
set +e
python3 "$repo/tools/windows-h264-recipe-transport.py" collect-tools --root "$root"
phase_exit=$?
set -e
finish_phase system-tools "$phase_exit"
mark_phase build-root-copy start
set +e
if [[ -d "$repo/.codex-target/ffmpeg-8.1.2" ]]; then cp -a "$repo/.codex-target/ffmpeg-8.1.2/." "$root/build-root/"; fi
phase_exit=$?
set -e
finish_phase build-root-copy "$phase_exit"
'@
        Write-ReleaseALf (Join-Path $Context.root 'setup/prepare.sh') $prepare
        Write-ReleaseALf (Join-Path $Context.root 'setup/collect.sh') $collect
        Write-ReleaseAJson $statePath $state
        $state.quiescent = $false
        Write-ReleaseAJson $statePath $state
        if ($aptDiagnostic) {
            $Context.setupCallId = [Guid]::NewGuid().ToString('N')
            Invoke-ReleaseAAptIdentity $Context $state
        }
        # Python是所选distro supervisor的显式前置；缺失即setup失败，不猜测或更换distro。
        $command = "bash --noprofile --norc $(Quote-ReleaseABash ($Context.wslRoot + '/setup/prepare.sh')) $(Quote-ReleaseABash $Context.wslRoot)"
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',
            "$($Context.wslRoot)/setup/lifecycle-supervisor.py",'--scope',"$($Context.wslRoot)/setup/groups/setup",'--nonce',$Context.setupNonce,
            '--call-id',$(if ($aptDiagnostic) { $Context.setupCallId } else { [Guid]::NewGuid().ToString('N') }),'run','--','bash','-lc',$command) -Directory $Context.repo `
            -RecordDirectory (Join-Path $Context.root 'setup/calls/setup') -TimeoutSeconds (Get-ReleaseASetupBudget $Context) -BeforeWindowsKill {
                Add-ReleaseAError $state 'setup' 'setup总预算超时'; Write-ReleaseAJson $statePath $state
                if ($aptDiagnostic) { Stop-ReleaseAScopes $Context -SetupOnly } else { Stop-ReleaseAScopes $Context } }
        Assert-ReleaseAResult $record '固定环境准备'
        if ($aptDiagnostic) { $state.apt_prefix_completed = $true; return }
        Invoke-ReleaseALinuxControls $Context
        $state.setup_ready = $true; $state.stage = 'builder'; $state.builder_started = $true
        Write-ReleaseAJson $statePath $state
        $runner = {
            param($Exe, $Arguments)
            Invoke-ReleaseARaw -Exe $Exe -Arguments $Arguments -Directory $Context.repo `
                -RecordDirectory (Join-Path $Context.root 'setup/calls/builder') -TimeoutSeconds 3600 `
                -BeforeWindowsKill {
                    Add-ReleaseAError $state 'builder' 'builder达到60分钟上限'; Write-ReleaseAJson $statePath $state; Stop-ReleaseAScopes $Context }
        }.GetNewClosure()
        $record = Invoke-ReleaseABuilderOnce $Context $runner
        $state.builder_exit = $record.exit
        $configuration = Join-Path $Context.repo '.codex-target/ffmpeg-8.1.2/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
        if (Test-Path -LiteralPath $configuration) {
            foreach ($receipt in Get-ChildItem -Path (Join-Path $configuration '.attempt-*/wsl-*.json') -File) {
                $call = Read-ReleaseAJson $receipt.FullName
                if ($null -ne $call.capture_error) { Add-ReleaseAError $state 'capture' $call.capture_error }
            }
        }
        Assert-ReleaseAResult $record '唯一builder'
    }
    catch {
        Add-ReleaseAError $state $state.stage $_.ToString()
        throw
    }
    finally {
        try {
            $diagnosticFailure = $null
            if ($aptDiagnostic -and $Context.ContainsKey('setupCallId')) {
                try { Stop-ReleaseAScopes $Context -SetupOnly; $state.quiescent = $true }
                catch {
                    if ($null -eq $state.first_error) { $diagnosticFailure = $_ }
                    Add-ReleaseAError $state 'apt_diagnostic_cleanup' $_.ToString()
                }
                try { Save-ReleaseAAptReceipts $Context }
                catch {
                    if ($null -eq $state.first_error) { $diagnosticFailure = $_ }
                    Add-ReleaseAError $state 'apt_diagnostic_export' $_.ToString()
                }
            }
            if ($null -ne $diagnosticFailure) { throw $diagnosticFailure }
        }
        finally { Write-ReleaseAJson $statePath $state }
    }
}
function Invoke-ReleaseAFailedWslExport($Context, $State) {
    $failedWslExporter = @'
"""只读本job唯一失败make调用；固定16目的，不是目录归档器。"""
import hashlib,json,os,re,stat,sys,time
from pathlib import Path

TOTAL_LIMIT = 40 * 1024 * 1024
META = 256 * 1024


def require(value, message):
    if not value:
        raise ValueError(message)


def location(value):
    text = str(value)
    require(len(text) <= 2048 and '\x00' not in text and not text.startswith(('\\\\', '//')), 'path syntax')
    require(not any(part in ('.', '..') for part in text.replace('\\', '/').split('/')), 'path escape')
    path = Path(text)
    require(path.is_absolute(), 'absolute path required')
    require(not any(':' in part for part in path.parts[1:]), 'ADS path')
    return path


def signature(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def regular(root, path, directory=False):
    root, path = location(root), location(path)
    relative = path.relative_to(root)  # native path rules, not string prefix
    current = root
    chain = [root] + [root.joinpath(*relative.parts[:i]) for i in range(1, len(relative.parts)+1)]
    for current in chain:
        info = current.lstat()
        require(not stat.S_ISLNK(info.st_mode) and not (getattr(info, 'st_file_attributes', 0) & 0x400), 'link/reparse refused')
        is_directory = current != path or directory
        require(stat.S_ISDIR(info.st_mode) if is_directory else stat.S_ISREG(info.st_mode), 'file type refused')
        if not is_directory:
            require(info.st_nlink == 1, 'hardlink refused')
    return info


def unique_json(raw):
    def pairs(items):
        require(len(dict(items)) == len(items), 'duplicate JSON key')
        return dict(items)
    return json.loads(raw, object_pairs_hook=pairs)


class Reader:
    def __init__(self):
        self.began = time.monotonic()
        self.sizes = {}
    def check(self):
        require(time.monotonic() - self.began < 20, 'export time bound')
    def before(self, root, path, cap):
        self.check()
        info = regular(root, path)
        require(info.st_size <= cap, 'file byte bound')
        self.sizes[str(path)] = info.st_size
        require(sum(self.sizes.values()) <= TOTAL_LIMIT, 'total byte bound')
        return info
    def read(self, root, path, cap):
        before = self.before(root, path, cap)
        with path.open('rb') as stream:
            require(signature(os.fstat(stream.fileno())) == signature(before), 'open identity changed')
            raw = stream.read(cap+1)
            require(len(raw) <= cap and signature(os.fstat(stream.fileno())) == signature(before), 'read changed')
        require(signature(regular(root, path)) == signature(before), 'read identity changed')
        return raw
    def json(self, root, path, cap=META):
        value = unique_json(self.read(root, path, cap))
        require(type(value) is dict, 'JSON object required')
        return value
    def copy(self, root, path, destination, cap):
        before = self.before(root, path, cap)
        first, second, count = hashlib.sha256(), hashlib.sha256(), 0
        with path.open('rb') as source, destination.open('xb') as output:
            require(signature(os.fstat(source.fileno())) == signature(before), 'copy open changed')
            while True:
                self.check()
                block = source.read(65536)
                if not block: break
                count += len(block); require(count <= cap, 'copy grew beyond bound')
                first.update(block); output.write(block)
            require(signature(os.fstat(source.fileno())) == signature(before), 'copy changed')
            output.flush(); os.fsync(output.fileno())
        with path.open('rb') as source:
            require(signature(os.fstat(source.fileno())) == signature(before), 'reread open changed')
            reread = 0
            while True:
                self.check()
                block = source.read(65536)
                if not block: break
                reread += len(block); require(reread <= cap, 'reread byte bound'); second.update(block)
            require(signature(os.fstat(source.fileno())) == signature(before), 'reread changed')
        require(signature(regular(root, path)) == signature(before) and count == before.st_size and first.digest() == second.digest(), 'copy identity/bytes changed')
        saved = hashlib.sha256(destination.read_bytes()).hexdigest()
        require(saved == first.hexdigest(), 'saved bytes changed')
        return dict(status='copied', original_path=str(path), size=count, source_sha256=first.hexdigest(), saved_sha256=saved,
                    read_boundary='bounded equal reads only; no global no-future-write assertion')


def entries(root, path, cap):
    before = regular(root, path, True)
    result = []
    with os.scandir(path) as stream:
        for item in stream:
            require(len(result) < cap, 'entry count bound')
            result.append(Path(item.path))
    require(signature(regular(root, path, True)) == signature(before), 'directory changed')
    return result


def option(argv, name):
    require(type(argv) is list and all(type(v) is str for v in argv) and argv.count(name) == 1, 'argv option binding')
    index = argv.index(name)
    require(index+1 < len(argv), 'argv value missing')
    return argv[index+1]


def export_failed(repo, evidence, source):
    repo, evidence = location(repo), location(evidence)
    regular(evidence.parent, evidence.parent, True)
    destination = evidence.parent / 'release-a-failed-wsl'
    destination.mkdir()  # existing output refuses; no overwrite/cleanup/retry
    result = dict(schema='freeremotedesk.release-a.failed-wsl.v1', complete=False, files={}, source=source)
    reader = Reader()
    try:
        state = reader.json(evidence, evidence/'provenance/state.json', 512*1024)
        require(re.fullmatch('[a-f0-9]{40}', source) and state['source'] == source and type(state['builder_exit']) is int and state['builder_exit'] != 0, 'failed builder source binding')
        outer = reader.json(evidence, evidence/'setup/calls/builder/exit.json', 512*1024)
        require(outer['terminated'] and outer['capture_complete'] and outer['exit'] != 0, 'Windows writer unproven')
        configuration = repo/'.codex-target/ffmpeg-8.1.2/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
        attempts = [p for p in entries(repo, configuration, 32) if p.name.startswith('.attempt-')]
        require(len(attempts) == 1, 'unique attempt required')
        attempt = attempts[0]
        match = re.fullmatch(r'\.attempt-([0-9]+)-[a-f0-9]{32}', attempt.name)
        require(match and int(match[1]) == outer['windows_child']['pid'], 'attempt PID binding')
        contents = entries(repo, attempt, 256)
        calls = [p for p in contents if p.name.startswith('wsl-') and p.name.endswith('.json')]
        require(0 < len(calls) <= 7 and all(re.fullmatch(r'wsl-[a-f0-9]{32}\.json', p.name) for p in calls), 'wsl receipt bound')
        records = [(p, reader.json(repo, p)) for p in calls]
        failed = [(p, v) for p, v in records if type(v.get('exit')) is not int or v['exit'] != 0 or v.get('capture_error') is not None]
        require(len(failed) == 1, 'unique failed wsl required')
        call_path, call = failed[0]
        require(call['action'] == 'FFmpeg build' and call['inner_argv'] == ['bash','-lc','make -j4'] and type(call['exit']) is int and call['exit'] != 0, 'only failed make call')
        intent = reader.json(repo, attempt/'lifecycle-intent.json')
        paths = reader.json(evidence, evidence/'provenance/paths.json')
        linux_attempt = paths['wsl'].rstrip('/') + '/' + attempt.relative_to(repo).as_posix()
        require(intent['scope_linux'] == linux_attempt+'/lifecycle' and intent['supervisor_linux'] == linux_attempt+'/release-a-supervisor.py', 'Linux scope path binding')
        nonce = intent['nonce']; require(re.fullmatch('[a-f0-9]{32}', nonce), 'nonce format')
        lifecycle = call['lifecycle']
        require(lifecycle == dict(nonce=nonce, scope=intent['scope_linux'], supervisor=intent['supervisor_linux'], source_sha256=intent['source_sha256']), 'wsl lifecycle binding')
        call_id = option(call['argv'], '--call-id')
        require(re.fullmatch('[a-f0-9]{32}', call_id) and call_path.name == 'wsl-'+call_id+'.json', 'call id binding')
        require(option(call['argv'], '--nonce') == nonce and option(call['argv'], '--scope') == intent['scope_linux'], 'wsl argv binding')
        stop_path = evidence/'setup/calls/linux-cleanup-builder/exit.json'
        stop_bytes = reader.read(evidence, stop_path, 512*1024)
        stop = unique_json(stop_bytes)
        require(stop['exit'] == 0 and stop['error'] is None and not stop['timed_out'] and stop['terminated'] and stop['capture_complete'], 'original stop gate')
        require(option(stop['argv'], '--nonce') == nonce and option(stop['argv'], '--scope') == intent['scope_linux'], 'stop nonce binding')
        raw_receipt = location(call['raw_receipt']); native = raw_receipt.parent
        require(native.parent == attempt and re.fullmatch('native-supervised-wsl-[a-f0-9]{32}', native.name) and raw_receipt.name == 'exit.json', 'native receipt escape')
        native_exit = reader.json(repo, raw_receipt, 512*1024)
        native_argv = reader.json(repo, native/'argv.json')
        require(native_exit['argv'] == native_argv['argv'] == call['argv'] and native_exit['cwd'] == native_argv['cwd'] == call['cwd'], 'original native argv/cwd binding')
        require(location(call['cwd']) == repo, 'original cwd binding')
        require(native_exit['terminated'] and native_exit['capture_complete'] and native_exit['exit'] == call['exit'], 'native exit binding')
        require(location(native_exit['receipt']) == raw_receipt, 'native self receipt')
        for name in ('stdout','stderr'):
            require(location(native_exit[name]) == location(call[name]) == native/(name+'.bin'), 'native stream escape')
        scope = attempt/'lifecycle'
        metadata = reader.json(repo, scope/'scope.json')
        require(metadata == dict(nonce=nonce, source_path=intent['supervisor_linux'], source_sha256=intent['source_sha256']), 'scope metadata binding')
        for name in ('halt','stopped'):
            value = reader.json(repo, scope/(name+'.json')); require(value['nonce'] == nonce, 'scope nonce')
            if name == 'stopped': require(value['quiescent'] and value['new_launches_blocked'], 'scope stop proof')
        plan = [('wsl.json',call_path,META), ('native.argv.json',native/'argv.json',META), ('native.exit.json',raw_receipt,512*1024),
                ('native.stderr.bin',native/'stderr.bin',16*1024*1024), ('native.stdout.bin',native/'stdout.bin',16*1024*1024),
                ('native.executable.bin',native/'executable.bin',4*1024*1024), ('supervisor.py.bin',attempt/'release-a-supervisor.py',META),
                ('lifecycle.intent.json',attempt/'lifecycle-intent.json',META), ('lifecycle.scope.json',scope/'scope.json',META),
                ('lifecycle.halt.json',scope/'halt.json',META), ('lifecycle.stopped.json',scope/'stopped.json',META)]
        for kind in ('pending','start','end','observations'):
            path = scope/(call_id+'.'+kind+'.json'); name = 'call.'+kind+'.json'
            if kind == 'observations' and not os.path.lexists(path):
                result['files'][name] = dict(status='missing-optional', original_path=str(path)); continue
            cap = 1024*1024 if kind == 'end' else META
            value = reader.json(repo, path, cap)
            require(value['nonce'] == nonce and value['call_id'] == call_id, 'UUID binding')
            if kind in ('pending','start'):
                require(value['source_sha256'] == intent['source_sha256'] and value['source_path'] == intent['supervisor_linux'], 'UUID source binding')
            plan.append((name,path,cap))
        for name,path,cap in plan:
            result['files'][name] = reader.copy(repo, path, destination/name, cap)
        require(result['files']['supervisor.py.bin']['source_sha256'] == intent['source_sha256'], 'supervisor bytes binding')
        executable = native_exit['executable']; sha = result['files']['native.executable.bin']['source_sha256']
        require(all(executable[k] == sha for k in ('before_sha256','saved_sha256','after_sha256')), 'native exe bytes binding')
        result.update(complete=True, attempt=attempt.name, nonce=nonce, call_id=call_id, original_stop_sha256=hashlib.sha256(stop_bytes).hexdigest())
    except Exception as error:
        result['error'] = str(error)[:2048]
    encoded = (json.dumps(result,sort_keys=True,indent=2)+'\n').encode()
    require(len(encoded) <= 128*1024, 'selection byte bound')
    with (destination/'selection.json').open('xb') as output: output.write(encoded)
    return result


if __name__ == '__main__':
    require(len(sys.argv) == 4, 'fixed repo/evidence/source only')
    result = export_failed(*sys.argv[1:])
    print(json.dumps(dict(complete=result['complete'], error=result.get('error'))))
    sys.exit(0 if result['complete'] else 1)
'@
    $script = Join-Path $env:RUNNER_TEMP 'release-a-failed-wsl-export.py'
    if (Test-Path -LiteralPath $script) { throw '本job export source已存在；不覆盖重试' }
    Write-ReleaseALf $script $failedWslExporter
    $expected = $failedWslExporter.Replace("`r`n","`n").TrimEnd([char[]]"`r`n") + "`n"
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($script)) -cne [Convert]::ToBase64String([Text.UTF8Encoding]::new($false).GetBytes($expected))) { throw '当前inline export bytes不匹配' }
    $record = Invoke-ReleaseARaw -Exe 'python' -Arguments @($script,$Context.repo,$Context.root,$env:GITHUB_SHA) -Directory $Context.repo `
        -RecordDirectory (Join-Path $env:RUNNER_TEMP 'release-a-failed-wsl-export-call') -TimeoutSeconds (Get-ReleaseACollectBudget $Context 20)
    Assert-ReleaseAResult $record '失败wsl有限原件导出'
}
function Get-ReleaseACollectBudget($Context, [int]$Cap = 300) {
    $remaining = [int][Math]::Floor(($Context.collectDeadline - [DateTime]::UtcNow).TotalSeconds)
    if ($remaining -le 0) { throw 'Collect原300秒总预算耗尽；不得新开窗口' }
    return [Math]::Min($Cap,$remaining)
}
function Invoke-ReleaseACollect($Context) {
    $Context.collectDeadline = [DateTime]::UtcNow.AddSeconds(300)
    $statePath = Join-Path $Context.root 'provenance/state.json'
    if (-not (Test-Path -LiteralPath $statePath)) { throw '初始化前失败，缺材料即不完整，不伪造artifact' }
    $state = Read-ReleaseAJson $statePath
    try {
        foreach ($name in @('setup','builder')) {
            $exitFile = Join-Path $Context.root "setup/calls/$name/exit.json"
            if (Test-Path -LiteralPath $exitFile) {
                $prior = Read-ReleaseAJson $exitFile
                if (-not $prior.terminated -or -not $prior.capture_complete) { throw 'Windows writer退出/原stream完整性未证明；禁止稳定封包' }
            }
        }
        $originalConfiguration = Join-Path $Context.repo '.codex-target/ffmpeg-8.1.2/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
        if (Test-Path -LiteralPath $originalConfiguration) {
            foreach ($attempt in Get-ChildItem -LiteralPath $originalConfiguration -Directory -Force -Filter '.attempt-*') {
                foreach ($native in Get-ChildItem -LiteralPath $attempt.FullName -Directory -Filter 'native-*') {
                    $nativeExit = Join-Path $native.FullName 'exit.json'
                    if (-not (Test-Path -LiteralPath $nativeExit)) { throw 'Windows native child终态缺失；保留forensic partial，拒绝stable' }
                    $record = Read-ReleaseAJson $nativeExit
                    if (-not $record.terminated -or -not $record.capture_complete) { throw 'Windows native child退出/EOF未证明；拒绝stable' }
                }
                foreach ($callPath in Get-ChildItem -LiteralPath $attempt.FullName -File -Filter 'wsl-*.json') {
                    $record = Read-ReleaseAJson $callPath.FullName
                    if (-not $record.terminated -or -not $record.capture_complete) { throw '原inner调用退出/EOF未证明；拒绝stable' }
                }
            }
        }
        Confirm-ReleaseAWindowsTools $Context
        if (-not $state.quiescent) { Stop-ReleaseAScopes $Context }
        $state.quiescent = $true
        if ($state.builder_started -and $null -ne $state.builder_exit -and $state.builder_exit -ne 0) {
            try { Invoke-ReleaseAFailedWslExport $Context $state }
            catch {
                # 独立旁路失败不改原builder first_error，也不替代原完整运输。
                Add-ReleaseAError $state 'forensic' $_.ToString()
                Write-ReleaseAJson $statePath $state
            }
        }
        $pathsFile = Join-Path $Context.root 'provenance/paths.json'
        if (Test-Path -LiteralPath $pathsFile) {
            $paths = Read-ReleaseAJson $pathsFile
            $nonce = [Guid]::NewGuid().ToString('N')
            New-Item -ItemType Directory -Force -Path (Join-Path $Context.root 'setup/groups/collect') | Out-Null
            Write-ReleaseAJson (Join-Path $Context.root 'setup/collect-intent.json') @{ nonce = $nonce
                supervisor_linux = "$($paths.evidence_wsl)/setup/lifecycle-supervisor.py"; scope_linux = "$($paths.evidence_wsl)/setup/groups/collect"
                source_sha256 = (Get-FileHash -LiteralPath (Join-Path $Context.root 'setup/lifecycle-supervisor.py') -Algorithm SHA256).Hash.ToLowerInvariant() }
            $state.quiescent = $false
            Write-ReleaseAJson $statePath $state
            $command = "bash --noprofile --norc $(Quote-ReleaseABash ($paths.evidence_wsl + '/setup/collect.sh')) $(Quote-ReleaseABash $paths.evidence_wsl) $(Quote-ReleaseABash $paths.wsl)"
            $args = @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',"$($paths.evidence_wsl)/setup/lifecycle-supervisor.py",
                '--scope',"$($paths.evidence_wsl)/setup/groups/collect",'--nonce',$nonce,'--call-id',([Guid]::NewGuid().ToString('N')),
                'run','--','bash','-lc',$command)
            $result = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $args -Directory $Context.repo `
                -RecordDirectory (Join-Path $Context.root 'setup/calls/collect') -TimeoutSeconds (Get-ReleaseACollectBudget $Context) `
                -BeforeWindowsKill { Stop-ReleaseAScopes $Context }
            Assert-ReleaseAResult $result '全量原件收集'
            Stop-ReleaseAScopes $Context
            $state.quiescent = $true
        }
        $configuration = Join-Path $Context.root 'build-root/build-side-frd-h264dc-aarch64-v1/windows-aarch64/Release'
        if (Test-Path -LiteralPath $configuration) {
            $attempts = @(Get-ChildItem -LiteralPath $configuration -Directory -Force -Filter '.attempt-*')
            if ($attempts.Count -gt 1) { throw '运输根存在多个attempt' }
            if ($attempts.Count -eq 1) { $state.attempt = $attempts[0].Name }
        }
        Write-ReleaseAJson $statePath $state
        $tar = Join-Path $env:RUNNER_TEMP 'release-a.tar'
        $receipt = Join-Path $env:RUNNER_TEMP 'release-a.receipt.json'
        if (Test-Path -LiteralPath $pathsFile) {
            $Context.transportDeadline = [DateTime]::UtcNow.AddMinutes(10)
            $args = @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',"$($paths.wsl)/tools/windows-h264-recipe-transport.py",
                'seal','--root',$paths.evidence_wsl,'--output',($paths.evidence_wsl + '/../release-a.tar'),
                '--receipt',($paths.evidence_wsl + '/../release-a.receipt.json'),'--expected-inputs',
                ($paths.evidence_wsl + '/checkout-inputs/blob-identities.json'))
            # pack日志放payload外，避免封装中自身变化；同job本地核对成功才上传。
            $result = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments $args -Directory $Context.repo `
                -RecordDirectory (Join-Path $env:RUNNER_TEMP 'release-a-pack') -TimeoutSeconds (Get-ReleaseATransportBudget $Context)
            Assert-ReleaseAResult $result 'POSIX全量封包与只读复核'
            Complete-ReleaseATransport $Context $paths $receipt (Join-Path $env:RUNNER_TEMP 'release-a-pack')
        }
        else { throw '没有可用WSL路径，保留原始partial；无法生成POSIX运输即不完整' }
    }
    catch {
        # 原stop/退出门未通过时不执行export；只留固定旁路的未导出事实。
        $fallback = Join-Path $env:RUNNER_TEMP 'release-a-failed-wsl'
        if (-not (Test-Path -LiteralPath $fallback)) {
            try {
                New-Item -ItemType Directory -Path $fallback | Out-Null
                Write-ReleaseAJson (Join-Path $fallback 'selection.json') @{ schema = 'freeremotedesk.release-a.failed-wsl.v1'
                    complete = $false; error = 'original Collect gate failed before export; no bypass'; files = @{} }
            }
            catch { [Console]::Error.WriteLine('fixed fallback summary保存失败；保留原错误') }
        }
        Add-ReleaseAError $state 'transport' $_.ToString()
        Write-ReleaseAJson $statePath $state
        throw
    }
}

# Dot-source只装载固定函数供Pester；正常CLI没有任意tool/recipe/命令输入。
if ($MyInvocation.InvocationName -ne '.') {
    if (-not (Test-ReleaseAAuthority $env:GITHUB_EVENT_NAME $env:GITHUB_REF ($env:RELEASE_A_EXECUTE -ceq 'true'))) {
        throw '未获明确A执行authority；下载/WSL/builder前拒绝'
    }
    if ($env:RUNNER_OS -cne 'Windows' -or $env:RUNNER_ARCH -cne 'X64' -or $env:GITHUB_SHA -cnotmatch '^[0-9a-f]{40}$' -or
        $env:RELEASE_A_WORKFLOW_SHA -cne $env:GITHUB_SHA) { throw '固定Windows/source/workflow身份不匹配' }
    try {
        $context = Get-ReleaseAContext
        if ($Action -ceq 'BootstrapDiagnostic') { Invoke-ReleaseABootstrapDiagnostic $context | Out-Null }
        elseif ($Action -ceq 'BootstrapCheckoutDiagnostic') {
            $context.bootstrapCheckoutDiagnostic = $true
            Invoke-ReleaseAExecute $context
        }
        elseif ($Action -ceq 'BootstrapAptDiagnostic') {
            $context.bootstrapAptDiagnostic = $true
            Invoke-ReleaseAExecute $context
        }
        elseif ($Action -ceq 'Execute') { Invoke-ReleaseAExecute $context }
        else { Invoke-ReleaseACollect $context }
    }
    catch {
        $original=$_
        try { Write-ReleaseABootstrapFirstError (Join-Path $env:RUNNER_TEMP 'release-a-bootstrap-error.json') $original $Action }
        catch { [Console]::Error.WriteLine('bootstrap外层原ErrorRecord保存失败（不覆盖原异常）：' + $_.ToString()) }
        throw $original
    }
}
