[CmdletBinding()]
param(
    [ValidateSet("Release", "Debug")]
    [string]$Configuration = "Release",
    [ValidateSet("x86_64", "x86", "arm64")]
    [string]$Architecture = "x86_64",
    [string]$WslDistribution = "ubuntu24.04",
    [string]$SourceRecipe = "upstream",
    [switch]$ForceRebuild,
    [switch]$RunNativeTests
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# 所有拒绝必须发生在下载、build tree 或发布副作用之前。
if ($SourceRecipe -cnotin @("upstream", "frd-h264dc-aarch64-v1")) {
    throw "未知 Windows source recipe: $SourceRecipe"
}
$ModifiedRecipe = $SourceRecipe -ceq "frd-h264dc-aarch64-v1"
if ($ModifiedRecipe -and $Architecture -cne "arm64") {
    throw "修改 Windows source recipe 仅允许 ARM64；ARM64EC 不支持"
}
if ($ModifiedRecipe -and $RunNativeTests) {
    throw "本配方仅生成 build-side 材料；原生数值/guard/qmul/QPC 与修改包资格尚未释放"
}

$Version = "8.1.2"
$ArchiveSha256 = "464BEB5E7BF0C311E68B45AE2F04E9CC2AF88851ABB4082231742A74D97B524C"
$ReleaseFingerprint = "FCF986EA15E6E293A5644F10B4322F04D67658D8"
$SourceUrl = "https://ffmpeg.org/releases/ffmpeg-$Version.tar.xz"
$SignatureUrl = "$SourceUrl.asc"
$KeyUrl = "https://ffmpeg.org/ffmpeg-devel.asc"
$ApprovedBundleFiles = @("avcodec-62.dll", "avutil-60.dll", "freeremotedesk_ffmpeg.dll")
$CorrespondingSourcePackageName = "FreeRemoteDesk-ffmpeg-$Version-corresponding-source.zip"

function Get-WindowsFfmpegArchitectureProfile([string]$Name) {
    switch ($Name) {
        "x86_64" {
            return [pscustomobject]@{
                Architecture = "x86_64"
                PlatformDirectory = "windows-x86_64"
                FfmpegArch = "x86_64"
                CrossPrefix = "x86_64-w64-mingw32-"
                CrossCompiler = "x86_64-w64-mingw32-gcc"
                RustTarget = "x86_64-pc-windows-msvc"
                MsvcArch = "x64"
                Machine = "x64"
                AssemblyKind = "x86asm"
                AssemblyProvenance = "nasm=NASM version 2.16.01"
                AssemblyGate = "have_x86asm=1"
                RequiresX86Asm = $true
            }
        }
        "x86" {
            return [pscustomobject]@{
                Architecture = "x86"
                PlatformDirectory = "windows-x86"
                FfmpegArch = "x86"
                CrossPrefix = "i686-w64-mingw32-"
                CrossCompiler = "i686-w64-mingw32-gcc"
                RustTarget = "i686-pc-windows-msvc"
                MsvcArch = "x86"
                Machine = "x86"
                AssemblyKind = "x86asm"
                AssemblyProvenance = "nasm=NASM version 2.16.01"
                AssemblyGate = "have_x86asm=1"
                RequiresX86Asm = $true
            }
        }
        "arm64" {
            return [pscustomobject]@{
                Architecture = "arm64"
                PlatformDirectory = "windows-aarch64"
                FfmpegArch = "aarch64"
                CrossPrefix = "/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-"
                CrossCompiler = "/opt/frd-llvm-mingw-20250709/bin/aarch64-w64-mingw32-clang"
                RustTarget = "aarch64-pc-windows-msvc"
                MsvcArch = "arm64"
                Machine = "arm64"
                AssemblyKind = "aarch64-neon"
                AssemblyProvenance = "aarch64-neon=FFmpeg AArch64/NEON"
                AssemblyGate = "have_neon=1"
                RequiresX86Asm = $false
            }
        }
        default { throw "不支持的 Windows FFmpeg 架构: $Name" }
    }
}

$ArchitectureProfile = Get-WindowsFfmpegArchitectureProfile $Architecture
$PlatformDirectory = $ArchitectureProfile.PlatformDirectory
$RequiresExplicitCargoTarget = $Architecture -ne "x86_64"

$RepoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
Import-Module (Join-Path $PSScriptRoot "ffmpeg-build-common.psm1") -Force
$BuildRoot = Join-Path $RepoRoot ".codex-target\ffmpeg-$Version"
$SourceCache = Join-Path $BuildRoot "source-cache"
$GpgHome = Join-Path $BuildRoot "gnupg"
$ConfigurationRoot = Join-Path $BuildRoot "$PlatformDirectory\$Configuration"
if ($ModifiedRecipe) {
    $ConfigurationRoot = Join-Path $BuildRoot "build-side-$SourceRecipe\$PlatformDirectory\$Configuration"
}
$CodecDir = Join-Path $ConfigurationRoot "codec"
$ReleaseAssetsDirectory = Join-Path $BuildRoot "release-assets"
$Archive = Join-Path $SourceCache "ffmpeg-$Version.tar.xz"
$Signature = "$Archive.asc"
$SigningKey = Join-Path $SourceCache "ffmpeg-devel.asc"
$ProvenanceLog = Join-Path $ConfigurationRoot "build-provenance.txt"
$ThirdPartyRoot = Join-Path $RepoRoot "third_party\ffmpeg\$Version"
$AttemptId = "$PID-$([Guid]::NewGuid().ToString('N'))"
$AttemptRoot = Join-Path $ConfigurationRoot ".attempt-$AttemptId"
$SourceParent = Join-Path $AttemptRoot "source"
$SourceDir = Join-Path $SourceParent "ffmpeg-$Version"
$DistDir = Join-Path $AttemptRoot "dist"
$CodecStage = Join-Path $ConfigurationRoot ".codec.stage-$AttemptId"
$ConfigureRecord = Join-Path $AttemptRoot "configure-command.txt"
$BuildLockPath = Join-Path $BuildRoot "$PlatformDirectory-$Configuration.lock"
if ($ModifiedRecipe) {
    $BuildLockPath = Join-Path $ConfigurationRoot "build-side.lock"
}

function Assert-UnderBuildRoot([string]$Path) {
    Assert-PathUnderRoot -Path $Path -AllowedRoot $BuildRoot
}

function Download-IfMissing([string]$Url, [string]$Destination) {
    if (Test-Path -LiteralPath $Destination) {
        return
    }
    $parent = Split-Path -Parent $Destination
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $temporary = "$Destination.download"
    Assert-UnderBuildRoot $temporary
    if ($ModifiedRecipe) {
        $result = Invoke-ModifiedNative 'curl.exe' @('--fail','--location','--proto','=https','--tlsv1.2','--output',$temporary,$Url) 'download'
        if ($result.exit -ne 0) { throw "下载失败: $Url" }
    }
    else {
    & curl.exe --fail --location --proto '=https' --tlsv1.2 --output $temporary $Url
    if ($LASTEXITCODE -ne 0) {
        throw "下载失败: $Url"
    }
    }
    Move-Item -LiteralPath $temporary -Destination $Destination
}

function Find-Gpg {
    $command = Get-Command gpg.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }
    $gitGpg = "C:\Program Files\Git\usr\bin\gpg.exe"
    if (Test-Path -LiteralPath $gitGpg) {
        return $gitGpg
    }
    throw "未找到 GPG；无法验证 FFmpeg 发布签名"
}

function Invoke-ModifiedNative([string]$Exe, [string[]]$Arguments, [string]$Name) {
    if (-not $ModifiedRecipe -or $Name -cnotin @('download','gpg-import','gpg-fingerprint','gpg-verify','wslpath','required-tools','supervised-wsl')) {
        throw '只允许modified既有固定native入口'
    }
    $directory = Join-Path $AttemptRoot ("native-" + $Name + '-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $directory | Out-Null
    $resolved = (Get-Command $Exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $record = @{ argv = @($Exe) + $Arguments; cwd = (Get-Location).ProviderPath; exit = $null
        category = $(if ($Name -cin @('wslpath','required-tools')) { 'readonly-bootstrap-no-linux-writer' } elseif ($Name -ceq 'supervised-wsl') { 'supervised-linux-writer' } else { 'owned-windows-child' })
        stdout = (Join-Path $directory 'stdout.bin'); stderr = (Join-Path $directory 'stderr.bin')
        receipt = (Join-Path $directory 'exit.json'); started_utc = [DateTime]::UtcNow.ToString('o')
        capture_error = $null; capture_complete = $false; main_module = @{ observed = $false; reason = '进程过快或MainModule不可观察' } }
    $record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $directory 'argv.json') -Encoding utf8NoBOM
    $process = [Diagnostics.Process]::new()
    $info = [Diagnostics.ProcessStartInfo]::new($Exe)
    $info.WorkingDirectory = $record.cwd
    $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process.StartInfo = $info
    $out = $null; $err = $null; $outTask = $null; $errTask = $null; $started = $false
    try {
        $record.executable = @{ invoked_name = $Exe; path = [IO.Path]::GetFullPath($resolved)
            before_sha256 = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant() }
        [IO.File]::Copy($resolved, (Join-Path $directory 'executable.bin'), $false)
        $record.executable.saved_sha256 = (Get-FileHash -LiteralPath (Join-Path $directory 'executable.bin') -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($record.executable.before_sha256 -cne $record.executable.saved_sha256) { throw 'native原exe复制中变化' }
        $out = [IO.File]::Open((Join-Path $directory 'stdout.bin'), 'CreateNew', 'Write', 'Read')
        $err = [IO.File]::Open((Join-Path $directory 'stderr.bin'), 'CreateNew', 'Write', 'Read')
        if (-not $process.Start()) { throw 'native child未启动' }
        $started = $true
        $outTask = $process.StandardOutput.BaseStream.CopyToAsync($out)
        $errTask = $process.StandardError.BaseStream.CopyToAsync($err)
        $record.windows_child = @{ pid = $process.Id; start_time_utc = $null; start_time_observed = $false }
        try { $record.windows_child.start_time_utc = $process.StartTime.ToUniversalTime().ToString('o'); $record.windows_child.start_time_observed = $true }
        catch { $record.windows_child.reason = $_.ToString() }
        try {
            $image = $process.MainModule.FileName
            if (-not [string]::IsNullOrWhiteSpace($image)) { $record.main_module = @{ observed = $true; path = $image } }
        }
        catch { $record.main_module.reason = $_.ToString() }
        $process.WaitForExit()
        $record.exit = $process.ExitCode
        $drains = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($outTask, $errTask))
        if (-not $drains.Wait(15000)) { throw 'native双stream未到EOF' }
        $drains.GetAwaiter().GetResult()
        $out.Flush($true); $err.Flush($true); $record.capture_complete = $true
    }
    catch { $record.capture_error = $_.ToString(); $record.error_detail = $_.Exception.ToString() }
    finally {
        if ($started -and $process.HasExited) { $record.exit = $process.ExitCode }
        $record.terminated = $started -and $process.HasExited
        $record.ended_utc = [DateTime]::UtcNow.ToString('o')
        if ($null -ne $outTask -and $null -ne $errTask -and -not $record.capture_complete) {
            try {
                $drains = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($outTask, $errTask))
                if (-not $drains.Wait(15000)) { throw 'native失败收尾原streams未到EOF' }
                $drains.GetAwaiter().GetResult()
                $out.Flush($true); $err.Flush($true); $record.capture_complete = $true
            }
            catch { if ($null -eq $record.capture_error) { $record.capture_error = $_.ToString() } }
        }
        try {
            $record.executable.after_sha256 = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($record.executable.after_sha256 -cne $record.executable.before_sha256) { throw 'native调用前后exe变化' }
            if ($record.main_module.observed -and -not [string]::Equals($record.main_module.path, $record.executable.path, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'native实际MainModule与原exe不同，拒绝身份'
            }
        }
        catch { if ($null -eq $record.capture_error) { $record.capture_error = $_.ToString() } }
        if ($null -ne $out) { $out.Dispose() }; if ($null -ne $err) { $err.Dispose() }
        $record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $directory 'exit.json') -Encoding utf8NoBOM
        $process.Dispose()
    }
    if ($null -ne $record.capture_error) { throw "native原通道捕获失败：$($record.capture_error)" }
    return $record
}

function Convert-ToWslPath([string]$Path) {
    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    $escaped = $Path.Replace("'", $singleQuoteEscape)
    if ($ModifiedRecipe) {
        $call = Invoke-ModifiedNative 'wsl.exe' @('--distribution',$WslDistribution,'--','bash','-lc',"wslpath -a '$escaped'") 'wslpath'
        $result = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($call.stdout))
        if ($call.exit -ne 0 -or [string]::IsNullOrWhiteSpace($result)) { throw "无法转换 WSL 路径: $Path" }
    }
    else {
    $result = & wsl.exe --distribution $WslDistribution -- bash -lc "wslpath -a '$escaped'" 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($result)) {
        throw "无法转换 WSL 路径: $Path"
    }
    }
    return $result.Trim()
}

function Invoke-Wsl([string]$WorkingDirectory, [string]$Command, [string]$Action) {
    if ($ModifiedRecipe) {
        if ((Test-Path -LiteralPath (Join-Path $AttemptRoot 'lifecycle-halt')) -or (Test-Path -LiteralPath (Join-Path $ConfigurationRoot 'lifecycle-halt'))) { throw 'attempt已HALT；禁止新launch' }
        $callId = [Guid]::NewGuid().ToString('N')
        if ($null -eq (Get-Variable -Name ReleaseALifecycleNonce -Scope Script -ErrorAction SilentlyContinue)) {
            $script:ReleaseALifecycleNonce = [Guid]::NewGuid().ToString('N')
        }
        $lifecycleSource = @'
#!/usr/bin/env python3
"""仅当前受信checkout生成的 RELEASE A Linux scope；不是包内可执行 verifier。"""
import argparse
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def require(value, message):
    if not value:
        raise RuntimeError(message)


def write(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + '.writing')
    with temporary.open('x') as stream:
        json.dump(value, stream, sort_keys=True, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


class OwnershipUncertain(RuntimeError):
    """当前扫描不能授予signal/seal；只能在既定预算内重新只读观察。"""


def identity(pid):
    path = Path('/proc', str(pid))
    first = (path / 'stat').read_text()
    fields = first[first.rindex(')') + 2:].split()
    uid_lines = [line.split()[1:] for line in (path / 'status').read_text().splitlines() if line.startswith('Uid:')]
    require(len(uid_lines) == 1 and len(uid_lines[0]) == 4, '实际/proc/status Uid缺失')
    uids = [int(value) for value in uid_lines[0]]
    again = (path / 'stat').read_text()
    after = again[again.rindex(')') + 2:].split()
    if any(fields[i] != after[i] for i in (1, 2, 3, 19)):
        raise OwnershipUncertain('读取/proc身份时父系/租约变化')
    return {'pid': pid, 'ppid': int(fields[1]), 'pgid': int(fields[2]), 'sid': int(fields[3]),
            'starttime': int(fields[19]), 'state': after[0], 'uid': uids[1], 'uid_values': uids}


def same(actual, expected):
    return all(actual[k] == expected[k] for k in ('pid', 'pgid', 'sid', 'starttime', 'uid'))


def table():
    result = {}
    for path in Path('/proc').iterdir():
        if path.name.isdigit():
            try:
                result[int(path.name)] = identity(int(path.name))
            except (FileNotFoundError, ProcessLookupError):
                pass
    return result


def spawn_gated(argv, cwd, stdout_fd=None, stderr_fd=None):
    """共享POSIX子层：实际exec前等待GO；Linux登记与租约由调用者负责。"""
    ready_r, ready_w = os.pipe()
    go_r, go_w = os.pipe()
    pid = os.fork()
    if pid == 0:
        try:
            os.close(ready_r)
            os.close(go_w)
            os.setsid()
            if stdout_fd is not None:
                os.dup2(stdout_fd, 1)
            if stderr_fd is not None:
                os.dup2(stderr_fd, 2)
            os.chdir(cwd)
            os.write(ready_w, json.dumps({'pid': os.getpid(), 'pgid': os.getpgrp(),
                                        'sid': os.getsid(0)}).encode() + b'\n')
            os.close(ready_w)
            require(os.read(go_r, 1) == b'G', '缺少登记后的 GO')
            os.close(go_r)
            os.execvp(argv[0], argv)
        except BaseException as error:
            os.write(2, ('supervisor child: ' + str(error)).encode())
            os._exit(126)
    os.close(ready_w)
    os.close(go_r)
    with os.fdopen(ready_r, 'rb') as ready:
        data = json.loads(ready.readline())
    require(data == {'pid': pid, 'pgid': pid, 'sid': pid}, 'setsid 启动身份不符')
    return pid, go_w


class Locked:
    def __init__(self, scope):
        self.path = Path(scope) / 'control.lock'
    def __enter__(self):
        self.stream = self.path.open('a+')
        fcntl.flock(self.stream, fcntl.LOCK_EX)
        return self
    def __exit__(self, *args):
        fcntl.flock(self.stream, fcntl.LOCK_UN)
        self.stream.close()


def metadata(scope, nonce):
    path = Path(scope) / 'scope.json'
    source = Path(__file__).absolute()
    expected = {'nonce': nonce, 'source_path': str(source),
                'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest()}
    if path.exists():
        require(json.loads(path.read_bytes()) == expected, 'scope nonce/source bytes/path 漂移')
    else:
        write(path, expected)
    return expected


def owned_graph(processes, children, leader, supervisor):
    require(leader['pid'] in processes and supervisor['pid'] in processes, '锚点租约缺失')
    require(same(processes[leader['pid']], leader) and same(processes[supervisor['pid']], supervisor), '锚点UID/租约变化')
    require(processes[leader['pid']]['ppid'] == supervisor['pid'], 'leader原父系变化')
    require(supervisor['pid'] != leader['pid'], 'supervisor不能为组内leader')
    if not children <= set(processes):
        raise OwnershipUncertain('直接收养children扫描不完整')
    group = {pid: item for pid, item in processes.items() if item['pgid'] == leader['pgid']}
    require(leader['pid'] in group, 'leader不在原组')
    for pid in children:
        require(processes[pid]['ppid'] == supervisor['pid'], '收养父系不匹配')
        require(pid in group and processes[pid]['sid'] == leader['sid'], '发现组外直接收养后代')
    # 包含所有当前后代，拒绝离开原SID/PGID的分支；不靠UID或进程名推断。
    descendants = {supervisor['pid']}
    for _ in range(len(processes) + 1):
        more = {pid for pid, item in processes.items() if item['ppid'] in descendants}
        if more <= descendants:
            break
        descendants |= more
    require(all(pid in group for pid in descendants - {supervisor['pid']}), '发现逃逸SID/PGID后代')
    for pid, item in group.items():
        require(item['sid'] == leader['sid'] and item['starttime'] >= leader['starttime'], '原组session/starttime不匹配')
        current, seen = item, set()
        while current['pid'] != supervisor['pid']:
            require(current['pid'] not in seen, '父系环路')
            seen.add(current['pid'])
            require(current['pgid'] == leader['pgid'] and current['sid'] == leader['sid'], '父系经过组外祖先')
            parent = processes.get(current['ppid'])
            if parent is None:
                raise OwnershipUncertain('父系祖先缺失，不能证明归属')
            require(parent['starttime'] <= current['starttime'], '父PID复用或父starttime晚于child')
            if parent['pid'] == supervisor['pid']:
                require(current['pid'] in children, '实际收养children缺少父系成员')
            current = parent
    return group


def stable_ownership(first, second, first_children, second_children, leader, supervisor):
    left = owned_graph(first, first_children, leader, supervisor)
    right = owned_graph(second, second_children, leader, supervisor)
    keys = ('pid', 'starttime', 'ppid', 'sid', 'pgid')
    if first_children != second_children or set(left) != set(right):
        raise OwnershipUncertain('两次快照成员/直接children集合不稳定')
    for pid in set(left) | {supervisor['pid']}:
        if any(first[pid][key] != second[pid][key] for key in keys):
            raise OwnershipUncertain('两次快照父系/identity不稳定')
    return [second[pid] for pid in sorted(right)]


def members(leader, supervisor):
    try:
        require(same(identity(supervisor['pid']), supervisor) and same(identity(leader['pid']), leader), '扫描前锚点租约变化')
        children_path = Path('/proc', str(supervisor['pid']), 'task', str(supervisor['pid']), 'children')
        def snapshot():
            before = {int(pid) for pid in children_path.read_text().split()}
            processes = table()
            after = {int(pid) for pid in children_path.read_text().split()}
            if before != after:
                raise OwnershipUncertain('扫描中直接children变化')
            return processes, after
        first, children_a = snapshot()
        second, children_b = snapshot()
        group = stable_ownership(first, second, children_a, children_b, leader, supervisor)
        # 最后重读父/子及两个锚点；不以/proc非原子扫描冒充PID租约。
        for expected in group + [supervisor]:
            actual = identity(expected['pid'])
            if any(actual[key] != expected[key] for key in ('pid', 'starttime', 'ppid', 'sid', 'pgid')):
                raise OwnershipUncertain('父/子最终重读不稳定')
        require(same(identity(supervisor['pid']), supervisor) and same(identity(leader['pid']), leader), '扫描后锚点UID/租约变化')
        if {int(pid) for pid in children_path.read_text().split()} != children_b:
            raise OwnershipUncertain('最终直接children变化')
        return group
    except (FileNotFoundError, ProcessLookupError) as error:
        raise OwnershipUncertain('扫描进程消失；当前不授signal/seal') from error


def signal_registered(start, sig):
    # 只有持有未reap子PID租约的原parent发送；外部stop只HALT并等待，避免check→helper死亡→PGID复用。
    require(os.getpid() == start['supervisor']['pid'], '只有持有leader租约的原supervisor可发信号')
    require(same(identity(start['supervisor']['pid']), start['supervisor']), 'supervisor 死亡/身份变化')
    require(same(identity(start['leader']['pid']), start['leader']), 'leader PID 租约丢失；不得凭旧PGID发信号')
    members(start['leader'], start['supervisor'])
    try:
        os.killpg(start['leader']['pgid'], sig)
    except ProcessLookupError:
        pass


def run(scope, nonce, call_id, argv):
    require(sys.platform == 'linux' and hasattr(os, 'waitid') and hasattr(os, 'WNOWAIT'),
            '真实 Linux prctl/proc/WNOWAIT 不可用')
    require(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0, 'PR_SET_CHILD_SUBREAPER 失败')
    scope = Path(scope)
    scope.mkdir(exist_ok=True)
    start_path, end_path = scope / (call_id + '.start.json'), scope / (call_id + '.end.json')
    pid = None
    error = None
    terminal = None
    stopped = False
    with Locked(scope):
        meta = metadata(scope, nonce)
        require(not (scope / 'halt.json').exists(), 'scope 已HALT；拒绝新launch')
        require(not start_path.exists() and not end_path.exists(), 'call id 重复')
        supervisor = identity(os.getpid())
        write(scope / (call_id + '.pending.json'), dict(meta, call_id=call_id, supervisor=supervisor))
        pid, go = spawn_gated(argv, Path.cwd())
        leader = identity(pid)
        require(leader['pid'] == leader['sid'] == leader['pgid'], 'leader session/PGID 不独立')
        start = dict(meta, call_id=call_id, inner_argv=argv, cwd=str(Path.cwd()),
                     leader=leader, supervisor=supervisor)
        write(start_path, start)
        os.write(go, b'G')
        os.close(go)
    try:
        cleanup_started = None
        term_sent = killed = False
        observed_difference = False
        last_uncertainty = None
        while True:
            # WNOWAIT保留leader，只有本parent在租约仍成立时可发组信号。
            terminal = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            halted = (scope / 'halt.json').exists()
            if halted or terminal is not None:
                if cleanup_started is None:
                    cleanup_started = time.monotonic()
                require(time.monotonic() - cleanup_started < 10, '有界观察/回收耗尽；' + str(last_uncertainty))
            try:
                with Locked(scope):
                    group = members(leader, supervisor)
                differences = [item for item in group if item['uid'] != leader['uid']]
                if differences and not observed_difference:
                    write(scope / (call_id + '.observations.json'), dict(nonce=nonce, call_id=call_id,
                        leader=leader, supervisor=supervisor, owned_uid_differences=differences))
                    observed_difference = True
                live = [item for item in group if item['state'] != 'Z']
                if terminal is not None and not live:
                    with Locked(scope):
                        final = members(leader, supervisor)
                        require(all(item['state'] == 'Z' for item in final), '最终仍有live成员')
                        for item in final:
                            if item['pid'] != pid and item['ppid'] == os.getpid():
                                os.waitpid(item['pid'], os.WNOHANG)
                        require(same(identity(pid), leader), '最终reap前leader租约变化')
                        _, status = os.waitpid(pid, 0)
                        stopped = True
                        exit_code = os.waitstatus_to_exitcode(status)
                        write(end_path, dict(nonce=nonce, call_id=call_id, quiescent=True, reaped=True,
                            child_exit=exit_code, error=error, last_uncertainty=last_uncertainty,
                            observed_uid_difference=observed_difference))
                    return exit_code if error is None else 124
                if halted or (terminal is not None and live):
                    if error is None:
                        error = 'scope HALT' if halted else 'leader退出后仍有同组写入后代'
                    if not term_sent:
                        with Locked(scope):
                            signal_registered(start, signal.SIGTERM)
                        term_sent = True
                    if not killed and time.monotonic() - cleanup_started > 2:
                        with Locked(scope):
                            signal_registered(start, signal.SIGKILL)
                        killed = True
            except OwnershipUncertain as uncertainty:
                # 只读重观测不扩大/重启任何进程；最后原因固定一个字段，不无限增长日志。
                last_uncertainty = str(uncertainty)
            time.sleep(.05)
    except BaseException as failure:
        error = str(failure)
        with Locked(scope):
            write(end_path, dict(nonce=nonce, call_id=call_id, quiescent=False, reaped=stopped,
                                 child_exit=None, error=error))
        raise


def stop(scope, nonce):
    require(sys.platform == 'linux', 'stop 必须在真实 Linux')
    scope = Path(scope)
    with Locked(scope):
        metadata(scope, nonce)
        if not (scope / 'halt.json').exists():
            write(scope / 'halt.json', {'nonce': nonce, 'reason': '当前job有界收尾；拒绝新launch'})
    began = time.monotonic()
    while True:
        pending = []
        with Locked(scope):
            for pending_path in scope.glob('*.pending.json'):
                require(pending_path.with_name(pending_path.name.replace('.pending.json', '.start.json')).exists(),
                        '登记前helper失败；无法证明无未登记child，拒绝seal')
            for path in sorted(scope.glob('*.start.json')):
                start = json.loads(path.read_bytes())
                require(start['nonce'] == nonce, 'call nonce 不匹配')
                end = path.with_name(path.name.replace('.start.json', '.end.json'))
                if end.exists():
                    record = json.loads(end.read_bytes())
                    require(record['nonce'] == nonce and record['quiescent'] and record['reaped'],
                            'supervisor 未证明停止，拒绝seal')
                    continue
                pending.append(start['call_id'])
                helper = identity(start['supervisor']['pid'])
                require(helper['state'] != 'Z' and same(helper, start['supervisor']), 'helper死亡/身份变化；拒绝seal')
                require(same(identity(start['leader']['pid']), start['leader']), 'leader租约丢失；拒绝seal')
                try:
                    members(start['leader'], start['supervisor'])
                except OwnershipUncertain:
                    pass  # 本轮不授seal；等原supervisor最终终态，12秒预算不变。
                # HALT已持锁写入；原parent负责有界TERM/KILL，外部绝不凭旧PGID发送。
            if not pending:
                write(scope / 'stopped.json', {'nonce': nonce, 'quiescent': True, 'new_launches_blocked': True})
                return
        require(time.monotonic() - began < 12, 'scope收尾超时；拒绝seal')
        time.sleep(.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('run', 'stop'))
    parser.add_argument('--scope', type=Path, required=True)
    parser.add_argument('--nonce', required=True)
    parser.add_argument('--call-id')
    parser.add_argument('inner', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    require(len(args.nonce) == 32 and all(c in '0123456789abcdef' for c in args.nonce), 'nonce格式错误')
    if args.action == 'stop':
        require(not args.inner, 'stop 不接受任意命令')
        stop(args.scope, args.nonce)
        return 0
    require(args.call_id and len(args.call_id) == 32 and all(c in '0123456789abcdef' for c in args.call_id) and args.inner[:2] == ['bash', '-lc'], 'run 参数错误')
    require(args.inner[:2] == ['bash', '-lc'] and len(args.inner) == 3, '只接受原inner bash -lc Command')
    return run(args.scope, args.nonce, args.call_id, args.inner)


if __name__ == '__main__':
    sys.exit(main())
'@
        $supervisor = Join-Path $AttemptRoot "release-a-supervisor.py"
        $supervisorBytes = [Text.UTF8Encoding]::new($false).GetBytes($lifecycleSource.Replace("`r`n", "`n").TrimEnd([char[]]"`r`n") + "`n")
        if (Test-Path -LiteralPath $supervisor) {
            if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($supervisor)) -cne [Convert]::ToBase64String($supervisorBytes)) {
                throw "同job supervisor bytes 变化；拒绝执行"
            }
        }
        else { [IO.File]::WriteAllBytes($supervisor, $supervisorBytes) }
        $supervisorWsl = Convert-ToWslPath $supervisor
        $scopeWsl = (Convert-ToWslPath $AttemptRoot) + "/lifecycle"
        $intentArgv = @("wsl.exe", "--distribution", $WslDistribution, "--cd", $WorkingDirectory, "--", "bash", "-lc", $Command)
        $actualArgs = @("--distribution", $WslDistribution, "--cd", $WorkingDirectory, "--", "python3", $supervisorWsl,
            "--scope", $scopeWsl, "--nonce", $script:ReleaseALifecycleNonce, "--call-id", $callId, "run", "--", "bash", "-lc", $Command)
        New-Item -ItemType Directory -Force -Path (Join-Path $AttemptRoot 'lifecycle') | Out-Null
        $intent = @{ nonce = $script:ReleaseALifecycleNonce; supervisor_linux = $supervisorWsl; scope_linux = $scopeWsl
            source_sha256 = (Get-FileHash -LiteralPath $supervisor -Algorithm SHA256).Hash.ToLowerInvariant() }
        $intentTemporary = Join-Path $AttemptRoot 'lifecycle-intent.writing'
        $intent | ConvertTo-Json | Set-Content -LiteralPath $intentTemporary -Encoding utf8NoBOM
        Move-Item -Force -LiteralPath $intentTemporary -Destination (Join-Path $AttemptRoot 'lifecycle-intent.json')
        $stdoutPath = Join-Path $AttemptRoot "wsl-$callId.stdout.bin"
        $stderrPath = Join-Path $AttemptRoot "wsl-$callId.stderr.bin"
        $recordPath = Join-Path $AttemptRoot "wsl-$callId.json"
        $record = @{ intent_argv = $intentArgv; argv = @("wsl.exe") + $actualArgs; cwd = (Get-Location).ProviderPath
            inner_argv = @("bash", "-lc", $Command); inner_cwd = $WorkingDirectory; action = $Action; exit = $null
            stdout = $stdoutPath; stderr = $stderrPath; capture_error = $null; capture_complete = $false; terminated = $false
            lifecycle = @{ nonce = $script:ReleaseALifecycleNonce; scope = $scopeWsl; supervisor = $supervisorWsl
                source_sha256 = (Get-FileHash -LiteralPath $supervisor -Algorithm SHA256).Hash.ToLowerInvariant() } }
        try {
            if ((Test-Path -LiteralPath (Join-Path $AttemptRoot 'lifecycle-halt')) -or (Test-Path -LiteralPath (Join-Path $ConfigurationRoot 'lifecycle-halt'))) { throw 'attempt已HALT；禁止新launch' }
            $raw = Invoke-ModifiedNative 'wsl.exe' $actualArgs 'supervised-wsl'
            $record.exit = $raw.exit; $record.capture_error = $raw.capture_error
            $record.capture_complete = $raw.capture_complete; $record.terminated = $raw.terminated
            $record.stdout = $raw.stdout; $record.stderr = $raw.stderr; $record.raw_receipt = $raw.receipt
        }
        catch { $record.capture_error = $_.ToString(); $record.error_detail = $_.Exception.ToString() }
        finally {
            $record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $recordPath -Encoding utf8NoBOM
        }
        if ($null -ne $record.capture_error) { throw "原stream capture失败；原记录 $recordPath；$($record.capture_error)" }
        if ($record.exit -ne 0) { throw "$Action 失败；原调用/分离bytes与退出保留于 $AttemptRoot" }
        return
    }
    & wsl.exe --distribution $WslDistribution --cd $WorkingDirectory -- bash -lc $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Action 失败"
    }
}

function Quote-Bash([string]$Value) {
    $singleQuoteEscape = "'" + '"' + "'" + '"' + "'"
    return "'" + $Value.Replace("'", $singleQuoteEscape) + "'"
}

function Find-VisualStudioTool([string]$Pattern) {
    $vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw "未找到 vswhere.exe；需要 Visual Studio 2022 C++ 工具链"
    }
    $tool = & $vswhere -latest -products * -find $Pattern | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($tool)) {
        throw "Visual Studio 中缺少工具: $Pattern"
    }
    return $tool
}

function Import-VisualStudioEnvironment([string]$DevCmd, [string]$MsvcArch) {
    $commandLine = "`"$DevCmd`" -no_logo -arch=$MsvcArch -host_arch=x64 >nul && set"
    $lines = & cmd.exe /d /s /c $commandLine
    if ($LASTEXITCODE -ne 0) {
        throw "无法初始化 Visual Studio $MsvcArch 环境"
    }
    foreach ($line in $lines) {
        $separator = $line.IndexOf('=')
        if ($separator -le 0) {
            continue
        }
        $name = $line.Substring(0, $separator)
        $value = $line.Substring($separator + 1)
        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
}

function Get-PeImports([string]$Dumpbin, [string]$File) {
    $output = & $Dumpbin /nologo /dependents $File
    if ($LASTEXITCODE -ne 0) {
        throw "无法检查 PE imports: $File"
    }
    $imports = @(
        $output | ForEach-Object {
            if ($_ -match '^\s+([A-Za-z0-9_.-]+\.dll)\s*$') {
                $Matches[1]
            }
        }
    )
    if ($imports.Count -eq 0) {
        throw "PE import 集合为空或无法解析: $File"
    }
    return $imports
}

function Assert-ExactPeImports(
    [string]$Dumpbin,
    [string]$File,
    [string[]]$ExpectedImports
) {
    $actual = @(Get-PeImports -Dumpbin $Dumpbin -File $File | Sort-Object)
    $expected = @($ExpectedImports | Sort-Object)
    $actualKey = (($actual | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    $expectedKey = (($expected | ForEach-Object { $_.ToLowerInvariant() }) -join "|")
    if ($actualKey -cne $expectedKey) {
        throw "PE imports 不匹配: $File；实际 [$($actual -join ', ')]，要求 [$($expected -join ', ')]"
    }
}

function Assert-ApprovedBundleImports([string]$Dumpbin, [string]$BundleDirectory) {
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "freeremotedesk_ffmpeg.dll") -ExpectedImports @(
        "avcodec-62.dll",
        "avutil-60.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
        "kernel32.dll",
        "ntdll.dll",
        "VCRUNTIME140.dll",
        "api-ms-win-crt-heap-l1-1-0.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll"
    )
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "avcodec-62.dll") -ExpectedImports @(
        "KERNEL32.dll",
        "msvcrt.dll",
        "avutil-60.dll"
    )
    Assert-ExactPeImports -Dumpbin $Dumpbin -File (Join-Path $BundleDirectory "avutil-60.dll") -ExpectedImports @(
        "bcrypt.dll",
        "KERNEL32.dll",
        "msvcrt.dll"
    )
}

New-Item -ItemType Directory -Force -Path $BuildRoot, $SourceCache, $GpgHome | Out-Null
if ($ModifiedRecipe) {
    New-Item -ItemType Directory -Force -Path $ConfigurationRoot | Out-Null
}
$buildLock = $null
Push-Location $RepoRoot
try {
    try {
        $buildLock = [IO.File]::Open(
            $BuildLockPath,
            [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::None
        )
    }
    catch {
        throw "同一 configuration 的 FFmpeg build 已在运行: $BuildLockPath"
    }

    if ($ModifiedRecipe) {
        New-Item -ItemType Directory -Path $AttemptRoot | Out-Null
    }
    Download-IfMissing $SourceUrl $Archive
    Download-IfMissing $SignatureUrl $Signature
    Download-IfMissing $KeyUrl $SigningKey

    $actualHash = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
    if ($actualHash -cne $ArchiveSha256) {
        throw "FFmpeg 源码 SHA-256 不匹配: $actualHash"
    }
    $signatureHash = (Get-FileHash -LiteralPath $Signature -Algorithm SHA256).Hash
    $signingKeyHash = (Get-FileHash -LiteralPath $SigningKey -Algorithm SHA256).Hash

    $gpg = Find-Gpg
    $gpgRelative = [IO.Path]::GetRelativePath($RepoRoot, $GpgHome).Replace('\', '/')
    $oldGpgHome = $env:GNUPGHOME
    $env:GNUPGHOME = $gpgRelative
    try {
        if ($ModifiedRecipe) {
            # argv/cwd/三次调用顺序保持；门禁使用stdout的独立解码副本，原双通道不拼接冒充交错序。
            $import = Invoke-ModifiedNative $gpg @('--batch','--no-autostart','--import',$SigningKey) 'gpg-import'
            $fingerprint = Invoke-ModifiedNative $gpg @('--batch','--no-autostart','--with-colons','--fingerprint','FFmpeg release signing key') 'gpg-fingerprint'
            $fingerprints = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($fingerprint.stdout))
            if ($fingerprints -notmatch "fpr:::::::::${ReleaseFingerprint}:") {
                throw "FFmpeg 发布密钥指纹不匹配"
            }
            $verification = Invoke-ModifiedNative $gpg @('--batch','--no-autostart','--status-fd','1','--verify',$Signature,$Archive) 'gpg-verify'
            $signatureStatus = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($verification.stdout))
            if ($verification.exit -ne 0 -or $signatureStatus -notmatch "VALIDSIG $ReleaseFingerprint ") {
                throw "FFmpeg 8.1.2 发布签名验证失败`n$signatureStatus"
            }
        }
        else {
        # 公钥验证无需 agent；精确 fingerprint 与 VALIDSIG 是不可省略的成功门禁。
        & $gpg --batch --no-autostart --import $SigningKey 2>&1 | Out-Host
        $fingerprints = (& $gpg --batch --no-autostart --with-colons --fingerprint "FFmpeg release signing key") -join "`n"
        if ($fingerprints -notmatch "fpr:::::::::${ReleaseFingerprint}:") {
            throw "FFmpeg 发布密钥指纹不匹配"
        }
        $signatureStatus = (& $gpg --batch --no-autostart --status-fd 1 --verify $Signature $Archive 2>&1) -join "`n"
        if ($LASTEXITCODE -ne 0 -or $signatureStatus -notmatch "VALIDSIG $ReleaseFingerprint ") {
            throw "FFmpeg 8.1.2 发布签名验证失败`n$signatureStatus"
        }
        }
    }
    finally {
        $env:GNUPGHOME = $oldGpgHome
    }

    New-Item -ItemType Directory -Force -Path $ConfigurationRoot, $AttemptRoot | Out-Null
    # 旧版脚本留下的 source/dist/configure cache 从不再使用；每次 build 都从签名归档 fresh 解压。
    foreach ($legacy in @(
        (Join-Path $ConfigurationRoot "source"),
        (Join-Path $ConfigurationRoot "dist"),
        (Join-Path $ConfigurationRoot "configure-command.txt")
    )) {
        if (Test-Path -LiteralPath $legacy) {
            Assert-UnderBuildRoot $legacy
            Remove-Item -LiteralPath $legacy -Recurse -Force
        }
    }
    if ($ForceRebuild -and -not $ModifiedRecipe) {
        Get-ChildItem -LiteralPath $ConfigurationRoot -Force | Where-Object {
            $_.Name -like '.attempt-*' -or $_.Name -like '.codec.stage-*' -or $_.Name -like '*.previous-*'
        } | Where-Object { $_.FullName -ne $AttemptRoot } | ForEach-Object {
            Assert-UnderBuildRoot $_.FullName
            Remove-Item -LiteralPath $_.FullName -Recurse -Force
        }
    }

    if ($ModifiedRecipe) {
        $sourceParentWsl = Convert-ToWslPath $SourceParent
        $archiveWsl = Convert-ToWslPath $Archive
        $sourceHelperWsl = Convert-ToWslPath (Join-Path $PSScriptRoot "desktop-ffmpeg-source.py")
        Invoke-Wsl (Convert-ToWslPath $RepoRoot) ("python3 " + (Quote-Bash $sourceHelperWsl) +
            " prepare --profile windows-aarch64 --source-recipe " + (Quote-Bash $SourceRecipe) +
            " --archive " + (Quote-Bash $archiveWsl) + " --destination " + (Quote-Bash $sourceParentWsl)) "固定 Windows source replay"
        $sourceAssetWsl = Convert-ToWslPath (Join-Path $AttemptRoot "FreeRemoteDesk-ffmpeg-$Version-windows-aarch64-$SourceRecipe-corresponding-source.zip")
        Invoke-Wsl (Convert-ToWslPath $RepoRoot) ("python3 " + (Quote-Bash $sourceHelperWsl) +
            " asset --profile windows-aarch64 --source-recipe " + (Quote-Bash $SourceRecipe) +
            " --archive " + (Quote-Bash $archiveWsl) + " --output " + (Quote-Bash $sourceAssetWsl)) "Windows 对应源码材料"
    }
    else {
        $SourceDir = Expand-VerifiedArchiveFresh `
            -Archive $Archive `
            -ExpectedArchiveSha256 $ArchiveSha256 `
            -DestinationParent $SourceParent `
            -ExpectedDirectoryName "ffmpeg-$Version" `
            -AllowedRoot $BuildRoot
    }

    $requiredWslTools = "command -v bash >/dev/null && command -v make >/dev/null && command -v $($ArchitectureProfile.CrossCompiler) >/dev/null"
    if ($ArchitectureProfile.RequiresX86Asm) {
        $requiredWslTools += " && command -v nasm >/dev/null"
    }
    if ($ModifiedRecipe) {
        $call = Invoke-ModifiedNative 'wsl.exe' @('--distribution',$WslDistribution,'--','bash','-lc',$requiredWslTools) 'required-tools'
        if ($call.exit -ne 0) {
        $requiredToolsText = "bash/make/$($ArchitectureProfile.CrossCompiler)"
        if ($ArchitectureProfile.RequiresX86Asm) {
            $requiredToolsText += "/nasm"
        }
        throw "WSL distribution '$WslDistribution' 缺少 $requiredToolsText"
        }
    }
    else {
    & wsl.exe --distribution $WslDistribution -- bash -lc $requiredWslTools
    if ($LASTEXITCODE -ne 0) {
        $requiredToolsText = "bash/make/$($ArchitectureProfile.CrossCompiler)"
        if ($ArchitectureProfile.RequiresX86Asm) {
            $requiredToolsText += "/nasm"
        }
        throw "WSL distribution '$WslDistribution' 缺少 $requiredToolsText"
    }
    }
    $sourceWsl = Convert-ToWslPath $SourceDir
    $distWsl = Convert-ToWslPath $DistDir
    $configureArgs = @(
        "--prefix=$distWsl",
        "--arch=$($ArchitectureProfile.FfmpegArch)",
        "--target-os=mingw32",
        "--cross-prefix=$($ArchitectureProfile.CrossPrefix)",
        "--cc=$($ArchitectureProfile.CrossCompiler)",
        "--disable-static",
        "--enable-shared",
        "--disable-programs",
        "--disable-doc",
        "--disable-everything",
        "--enable-decoder=hevc,h264",
        "--enable-parser=hevc,h264",
        "--enable-protocol=file",
        "--disable-gpl",
        "--disable-nonfree",
        "--disable-version3",
        "--disable-autodetect",
        "--disable-network"
    )
    if ($RequiresExplicitCargoTarget) {
        $configureArgs += "--enable-cross-compile"
    }
    if ($Architecture -eq "x86") {
        # i686 除法等编译器辅助例程静态链接，禁止新增 libgcc DLL 依赖。
        $configureArgs += "--extra-ldflags=-static-libgcc"
    }
    if (-not $ArchitectureProfile.RequiresX86Asm) {
        $configureArgs += "--disable-x86asm"
    }
    if ($Configuration -eq "Release") {
        $configureArgs += "--disable-debug", "--enable-stripping"
    }
    else {
        $configureArgs += "--enable-debug", "--disable-stripping"
    }
    if ($ModifiedRecipe) {
        $attemptWsl = Convert-ToWslPath $AttemptRoot
        $captureHelperWsl = Convert-ToWslPath (Join-Path $PSScriptRoot "windows-ffmpeg-build-capture.py")
        Invoke-Wsl $sourceWsl ("python3 " + (Quote-Bash $captureHelperWsl) +
            " initialize --attempt " + (Quote-Bash $attemptWsl) + " --tree " + (Quote-Bash $sourceWsl)) "自然工具捕获初始化"
        $wrapperPrefix = "$attemptWsl/tool-capture/bin/aarch64-w64-mingw32-"
        # 仅选择捕获入口；wrapper 以原 driver 名称/参数调用固定工具，不规范化自然 make argv。
        $configureArgs = @($configureArgs | Where-Object { -not $_.StartsWith("--cc=") })
        # 转义 dollar 穿过 configure append/eval，写出 $$@；再由原 library.mak eval 展开实际 DLL target。
        $configureArgs += "--cc=$($wrapperPrefix)clang", "--as=$($wrapperPrefix)clang", "--ld=$($wrapperPrefix)clang",
            "--strip=$($wrapperPrefix)strip", '--extra-ldsoflags=-Wl,--Map=\$\$@.map'
    }
    $configureCommand = "./configure " + (($configureArgs | ForEach-Object { Quote-Bash $_ }) -join " ")
    if ($ModifiedRecipe) {
        Set-Content -LiteralPath $ConfigureRecord -Value $configureCommand -Encoding UTF8
    }
    Invoke-Wsl $sourceWsl $configureCommand "FFmpeg configure"
    if (-not $ModifiedRecipe) {
        Set-Content -LiteralPath $ConfigureRecord -Value $configureCommand -Encoding UTF8
    }
    Invoke-Wsl $sourceWsl "make -j4" "FFmpeg build"
    Invoke-Wsl $sourceWsl "make install" "FFmpeg install"

    if ($ModifiedRecipe) {
        $bundleHelperWsl = Convert-ToWslPath (Join-Path $PSScriptRoot "windows-ffmpeg-bundle.py")
        Invoke-Wsl $sourceWsl ("python3 " + (Quote-Bash $bundleHelperWsl) +
            " complete --source-recipe " + (Quote-Bash $SourceRecipe) + " --architecture arm64 --attempt " +
            (Quote-Bash $attemptWsl) + " --tree " + (Quote-Bash $sourceWsl) + " --archive " +
            (Quote-Bash $archiveWsl) + " --dist " + (Quote-Bash $distWsl)) "Windows build-side identity"
        Write-Host "修改配方 build-side 材料已保留: $AttemptRoot；未取得 stage/install 或原生资格。"
        return
    }

    $configHeader = @(
        Get-Content -Raw -LiteralPath (Join-Path $SourceDir "config.h")
        Get-Content -Raw -LiteralPath (Join-Path $SourceDir "config_components.h")
    ) -join "`n"
    $requiredConfigDefines = @(
        "#define CONFIG_GPL 0",
        "#define CONFIG_NONFREE 0",
        "#define CONFIG_HEVC_DECODER 1",
        "#define CONFIG_HEVC_PARSER 1",
        "#define CONFIG_H264_DECODER 1",
        "#define CONFIG_H264_PARSER 1",
        "#define CONFIG_FILE_PROTOCOL 1"
    )
    if ($ArchitectureProfile.RequiresX86Asm) {
        $requiredConfigDefines += "#define HAVE_X86ASM 1"
    }
    else {
        $requiredConfigDefines += "#define HAVE_NEON 1"
    }
    foreach ($required in $requiredConfigDefines) {
        if (-not $configHeader.Contains($required)) {
            throw "FFmpeg 配置缺少必需门禁: $required"
        }
    }
    if ($ArchitectureProfile.RequiresX86Asm) {
        if ($configHeader -notmatch '(?m)^\s*#define HAVE_X86ASM 1\s*$') {
            throw "FFmpeg 配置缺少精确 x86asm 门禁: #define HAVE_X86ASM 1"
        }
    }
    elseif ($configHeader -notmatch '(?m)^\s*#define HAVE_NEON 1\s*$') {
        throw "FFmpeg 配置缺少精确 AArch64/NEON 门禁: #define HAVE_NEON 1"
    }
    $assemblyGate = $ArchitectureProfile.AssemblyGate

    $libExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\$($ArchitectureProfile.MsvcArch)\lib.exe"
    $clExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\$($ArchitectureProfile.MsvcArch)\cl.exe"
    $dumpbinExe = Find-VisualStudioTool "VC\Tools\MSVC\**\bin\Hostx64\$($ArchitectureProfile.MsvcArch)\dumpbin.exe"
    foreach ($library in @(
        @{ Name = "avutil"; Definition = "avutil-60.def" },
        @{ Name = "avcodec"; Definition = "avcodec-62.def" }
    )) {
        $definition = Join-Path $DistDir "lib\$($library.Definition)"
        $outputLibrary = Join-Path $DistDir "lib\$($library.Name).lib"
        & $libExe /nologo "/def:$definition" "/machine:$($ArchitectureProfile.Machine)" "/out:$outputLibrary"
        if ($LASTEXITCODE -ne 0) {
            throw "生成 MSVC import library 失败: $($library.Name)"
        }
    }

    $devCmd = Find-VisualStudioTool "Common7\Tools\VsDevCmd.bat"
    Import-VisualStudioEnvironment $devCmd $ArchitectureProfile.MsvcArch
    $env:FFMPEG_DIR = $DistDir
    $cargoTargetArguments = @()
    if ($RequiresExplicitCargoTarget) {
        $cargoTargetArguments = @("--target", $ArchitectureProfile.RustTarget)
    }
    if ($Configuration -eq "Release") {
        $cargoBuildArguments = @("build", "--locked") + $cargoTargetArguments + @("-p", "frd-video-ffmpeg-plugin", "--features", "native-ffmpeg", "--release")
        & cargo.exe @cargoBuildArguments
        $pluginRoot = if ($RequiresExplicitCargoTarget) { Join-Path $RepoRoot "target\$($ArchitectureProfile.RustTarget)\release" } else { Join-Path $RepoRoot "target\release" }
        $plugin = Join-Path $pluginRoot "freeremotedesk_ffmpeg.dll"
    }
    else {
        $cargoBuildArguments = @("build", "--locked") + $cargoTargetArguments + @("-p", "frd-video-ffmpeg-plugin", "--features", "native-ffmpeg")
        & cargo.exe @cargoBuildArguments
        $pluginRoot = if ($RequiresExplicitCargoTarget) { Join-Path $RepoRoot "target\$($ArchitectureProfile.RustTarget)\debug" } else { Join-Path $RepoRoot "target\debug" }
        $plugin = Join-Path $pluginRoot "freeremotedesk_ffmpeg.dll"
    }
    if ($LASTEXITCODE -ne 0) {
        throw "native FFmpeg plugin 构建失败"
    }

    New-Item -ItemType Directory -Force -Path $CodecStage | Out-Null
    Copy-Item -LiteralPath (Join-Path $DistDir "bin\avutil-60.dll") -Destination $CodecStage
    Copy-Item -LiteralPath (Join-Path $DistDir "bin\avcodec-62.dll") -Destination $CodecStage
    Copy-Item -LiteralPath $plugin -Destination $CodecStage
    Assert-ExactDirectoryFiles -Directory $CodecStage -ApprovedFileNames $ApprovedBundleFiles
    Assert-ApprovedBundleImports -Dumpbin $dumpbinExe -BundleDirectory $CodecStage

    $correspondingSourcePackage = New-CorrespondingSourcePackage `
        -SourceArchive $Archive `
        -ExpectedArchiveSha256 $ArchiveSha256 `
        -Signature $Signature `
        -ExpectedSignatureSha256 $signatureHash `
        -License (Join-Path $ThirdPartyRoot "LICENSE.LGPLv2.1") `
        -Changes (Join-Path $ThirdPartyRoot "changes.diff") `
        -ReleaseAssetsDirectory $ReleaseAssetsDirectory `
        -PackageFileName $CorrespondingSourcePackageName `
        -SourceUrl $SourceUrl `
        -AllowedRoot $BuildRoot

    Publish-ExactDirectory `
        -StagingDirectory $CodecStage `
        -DestinationDirectory $CodecDir `
        -ApprovedFileNames $ApprovedBundleFiles `
        -AllowedRoot $BuildRoot | Out-Null
    Assert-ExactDirectoryFiles -Directory $CodecDir -ApprovedFileNames $ApprovedBundleFiles
    Assert-ApprovedBundleImports -Dumpbin $dumpbinExe -BundleDirectory $CodecDir

    $toolCommand = "printf 'bash='; bash --version | head -1; printf 'make='; make --version | head -1; printf 'cc='; $($ArchitectureProfile.CrossCompiler) --version | head -1"
    if ($ArchitectureProfile.RequiresX86Asm) {
        $toolCommand += "; printf 'nasm='; nasm -v"
    }
    $toolProvenance = & wsl.exe --distribution $WslDistribution -- bash -lc $toolCommand
    $provenanceStage = "$ProvenanceLog.stage-$AttemptId"
    Assert-UnderBuildRoot $provenanceStage
    @(
        "source_url=$SourceUrl",
        "signature_url=$SignatureUrl",
        "signing_key_url=$KeyUrl",
        "archive_sha256=$actualHash",
        "signature_sha256=$signatureHash",
        "signing_key_sha256=$signingKeyHash",
        "release_fingerprint=$ReleaseFingerprint",
        "signature=VALID",
        "source_extraction=fresh-per-build",
        "wsl_distribution=$WslDistribution",
        "msvc_cl=$clExe",
        "msvc_cl_version=$((Get-Item -LiteralPath $clExe).VersionInfo.FileVersion)",
        "configuration=$Configuration",
        "platform=$PlatformDirectory",
        "assembly_kind=$($ArchitectureProfile.AssemblyKind)",
        "assembly_provenance=$($ArchitectureProfile.AssemblyProvenance)",
        "configure=$configureCommand",
        $assemblyGate,
        "codec_files=$($ApprovedBundleFiles -join ',')",
        "codec_imports=VALID",
        "corresponding_source_package=$correspondingSourcePackage",
        "corresponding_source_package_sha256=$((Get-FileHash -LiteralPath $correspondingSourcePackage -Algorithm SHA256).Hash)",
        $toolProvenance
    ) | Set-Content -LiteralPath $provenanceStage -Encoding UTF8
    Move-Item -Force -LiteralPath $provenanceStage -Destination $ProvenanceLog

    if ($RunNativeTests) {
        $oldPath = $env:PATH
        $env:PATH = "$CodecDir;$($env:PATH)"
        try {
            $cargoTestArguments = @("test", "--locked") + $cargoTargetArguments + @("-p", "frd-video-ffmpeg-plugin", "--features", "native-ffmpeg", "--lib", "--", "--nocapture")
            & cargo.exe @cargoTestArguments
            if ($LASTEXITCODE -ne 0) {
                throw "native FFmpeg plugin 单元测试失败"
            }
            $cargoFixtureArguments = @("test", "--locked") + $cargoTargetArguments + @("-p", "frd-video-ffmpeg-plugin", "--test", "main444_decode", "--", "--nocapture")
            & cargo.exe @cargoFixtureArguments
            if ($LASTEXITCODE -ne 0) {
                throw "Main444 fixture integration test 失败"
            }
        }
        finally {
            $env:PATH = $oldPath
        }
    }

    Write-Host "FFmpeg $Version LGPL shared build complete from a fresh verified extraction."
    Write-Host "Codec bundle: $CodecDir"
    foreach ($file in Get-ChildItem -LiteralPath $CodecDir -File | Sort-Object Name) {
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
        Write-Host "$($file.Name) $($file.Length) bytes SHA256=$hash"
    }
    Write-Host "Corresponding-source release asset: $correspondingSourcePackage"
}
catch {
    if ($ModifiedRecipe -and (Test-Path -LiteralPath $AttemptRoot)) {
        @{ status = "failed"; publishable = $false; recipe = $SourceRecipe; error = $_.Exception.Message } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $AttemptRoot "attempt-failure.json") -Encoding UTF8
    }
    throw
}
finally {
    try {
        $temporaryPaths = if ($ModifiedRecipe) { @($CodecStage) } else { @($AttemptRoot, $CodecStage) }
        if ($ModifiedRecipe) {
            Write-Host "保留修改配方 attempt（含失败 partial）: $AttemptRoot"
        }
        foreach ($temporary in $temporaryPaths) {
            if (Test-Path -LiteralPath $temporary) {
                Assert-UnderBuildRoot $temporary
                Remove-Item -LiteralPath $temporary -Recurse -Force
            }
        }
    }
    finally {
        if ($null -ne $buildLock) {
            $buildLock.Dispose()
        }
        Pop-Location
    }
}
