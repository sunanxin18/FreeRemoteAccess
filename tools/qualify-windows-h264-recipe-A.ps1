[CmdletBinding()]
param([ValidateSet('Execute', 'Collect')][string]$Action = 'Execute')
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
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue
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
        $resolved = (Get-Command $Exe -CommandType Application -ErrorAction Stop).Source
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
    if ($null -ne $Record.error -or $Record.timed_out -or $Record.exit -ne 0) { throw "$Name失败：$($Record | ConvertTo-Json -Compress)" }
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
function Stop-ReleaseAScopes($Context, [switch]$ControlsOnly) {
    # 只执行本checkout提取且已核对bytes的同job脚本，永不从下载artifact取可执行程序。
    $expected = (Get-ReleaseASupervisorSource $Context.repo).Replace("`r`n","`n").TrimEnd([char[]]"`r`n") + "`n"
    $scopes = @()
    $kinds = if ($ControlsOnly) { @('control-normal','control-halt') } else { @('setup','collect','control-normal','control-halt') }
    foreach ($kind in $kinds) {
        $intent = Join-Path $Context.root "setup/$kind-intent.json"
        if (Test-Path -LiteralPath $intent) { $scopes += @{ kind = $kind; intent = $intent
            source = (Join-Path $Context.root 'setup/lifecycle-supervisor.py') } }
    }
    if (-not $ControlsOnly) {
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
function Invoke-ReleaseAExecute($Context) {
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
DEBIAN_FRONTEND=noninteractive apt-get install --no-install-recommends -y python3 build-essential make patch pkg-config xz-utils tar curl ca-certificates procps util-linux
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
        $collect = @'
#!/bin/bash
set -euo pipefail
root=$1
repo=$2
if [[ -e /opt/frd-llvm-mingw-20250709 ]]; then cp -a /opt/frd-llvm-mingw-20250709 "$root/toolchain/install"; fi
python3 "$repo/tools/windows-h264-recipe-transport.py" collect-tools --root "$root"
if [[ -d "$repo/.codex-target/ffmpeg-8.1.2" ]]; then cp -a "$repo/.codex-target/ffmpeg-8.1.2/." "$root/build-root/"; fi
'@
        Write-ReleaseALf (Join-Path $Context.root 'setup/prepare.sh') $prepare
        Write-ReleaseALf (Join-Path $Context.root 'setup/collect.sh') $collect
        Write-ReleaseAJson $statePath $state
        $state.quiescent = $false
        Write-ReleaseAJson $statePath $state
        # Python是所选distro supervisor的显式前置；缺失即setup失败，不猜测或更换distro。
        $command = "bash --noprofile --norc $(Quote-ReleaseABash ($Context.wslRoot + '/setup/prepare.sh')) $(Quote-ReleaseABash $Context.wslRoot)"
        $record = Invoke-ReleaseARaw -Exe 'wsl.exe' -Arguments @('--distribution','Ubuntu-24.04','--user','root','--exec','python3',
            "$($Context.wslRoot)/setup/lifecycle-supervisor.py",'--scope',"$($Context.wslRoot)/setup/groups/setup",'--nonce',$Context.setupNonce,
            '--call-id',([Guid]::NewGuid().ToString('N')),'run','--','bash','-lc',$command) -Directory $Context.repo `
            -RecordDirectory (Join-Path $Context.root 'setup/calls/setup') -TimeoutSeconds (Get-ReleaseASetupBudget $Context) -BeforeWindowsKill {
                Add-ReleaseAError $state 'setup' 'setup总预算超时'; Write-ReleaseAJson $statePath $state; Stop-ReleaseAScopes $Context }
        Assert-ReleaseAResult $record '固定环境准备'
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
    finally { Write-ReleaseAJson $statePath $state }
}
function Invoke-ReleaseACollect($Context) {
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
                -RecordDirectory (Join-Path $Context.root 'setup/calls/collect') -TimeoutSeconds 300 `
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
    $context = Get-ReleaseAContext
    if ($Action -ceq 'Execute') { Invoke-ReleaseAExecute $context }
    else { Invoke-ReleaseACollect $context }
}
