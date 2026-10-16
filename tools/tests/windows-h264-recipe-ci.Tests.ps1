BeforeAll {
    . (Join-Path $PSScriptRoot '../qualify-windows-h264-recipe-A.ps1')
}
Describe 'Application resolution preserves the first natural candidate' {
    BeforeAll {
        $tokens=$null;$errors=$null
        $ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot '../build-ffmpeg-windows.ps1'),[ref]$tokens,[ref]$errors)
        if($errors.Count -ne 0){throw 'builder解析失败'}
        $node=$ast.Find({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -ceq 'Invoke-ModifiedNative'},$true)
        Invoke-Expression $node.Extent.Text
    }
    It 'uses the first of two candidates in the real catalog and both native capture gateways' {
        if(-not $IsWindows){Set-ItResult -Skipped -Because '实际Windows PowerShell专用';return}
        $script:resolutionExe=(Get-Process -Id $PID).Path
        $script:resolutionSecond=Join-Path $TestDrive 'second-never-selected.exe'
        [IO.File]::WriteAllBytes($script:resolutionSecond,[byte[]](0,255,17,34))
        # 仅mock命令发现边界；实际生产catalog、exe保存、raw/native helper与固定pwsh child都执行。
        Mock Get-Command {
            @([pscustomobject]@{Source=$script:resolutionExe},[pscustomobject]@{Source=$script:resolutionSecond})
        } -ParameterFilter {$CommandType -eq 'Application'}
        $catalogRoot=Join-Path $TestDrive 'resolution-catalog'
        New-Item -ItemType Directory -Path $catalogRoot | Out-Null
        Save-ReleaseAWindowsTools @{root=$catalogRoot}
        $catalog=Read-ReleaseAJson (Join-Path $catalogRoot 'toolchain/windows/catalog.json')
        $catalog.Keys.Count | Should -Be 6
        foreach($entry in $catalog.Values){$entry.path | Should -BeExactly $script:resolutionExe}
        $argsFixed=@('-NoProfile','-Command','exit 7')
        $raw=Invoke-ReleaseARaw -Exe $script:resolutionExe -Arguments $argsFixed -Directory $TestDrive `
            -RecordDirectory (Join-Path $TestDrive 'resolution-raw') -TimeoutSeconds 30
        $raw.error | Should -BeNullOrEmpty
        $raw.exit | Should -Be 7
        $raw.executable.path | Should -BeExactly $script:resolutionExe
        $raw.argv | Should -Be (@($script:resolutionExe)+$argsFixed)
        $raw.cwd | Should -BeExactly $TestDrive
        $ModifiedRecipe=$true;$AttemptRoot=Join-Path $TestDrive 'resolution-native'
        New-Item -ItemType Directory -Path $AttemptRoot | Out-Null
        $native=Invoke-ModifiedNative $script:resolutionExe $argsFixed 'gpg-import'
        $native.exit | Should -Be 7
        $native.capture_complete | Should -BeTrue
        $native.executable.path | Should -BeExactly $script:resolutionExe
        $native.argv | Should -Be (@($script:resolutionExe)+$argsFixed)
    }
    It 'keeps the original zero-candidate catalog error and Stop lookup failure semantics' {
        if(-not $IsWindows){Set-ItResult -Skipped -Because '实际Windows PowerShell专用';return}
        $exe=(Get-Process -Id $PID).Path
        Mock Get-Command {return} -ParameterFilter {$CommandType -eq 'Application' -and $ErrorAction -eq 'SilentlyContinue'}
        Mock Get-Command {throw [Management.Automation.CommandNotFoundException]::new('fixed-zero-candidate-marker')} `
            -ParameterFilter {$CommandType -eq 'Application' -and $ErrorAction -eq 'Stop'}
        $catalogRoot=Join-Path $TestDrive 'zero-catalog'
        New-Item -ItemType Directory -Path $catalogRoot | Out-Null
        {Save-ReleaseAWindowsTools @{root=$catalogRoot}} | Should -Throw '*缺少Windows固定工具 pwsh*'
        $raw=Invoke-ReleaseARaw -Exe $exe -Arguments @('-NoProfile','-Command','exit 7') -Directory $TestDrive `
            -RecordDirectory (Join-Path $TestDrive 'zero-raw') -TimeoutSeconds 30
        $raw.error | Should -Match 'fixed-zero-candidate-marker'
        $raw.exit | Should -BeNullOrEmpty
        $raw.terminated | Should -BeFalse
        $raw.argv[0] | Should -BeExactly $exe
        $ModifiedRecipe=$true;$AttemptRoot=Join-Path $TestDrive 'zero-native'
        New-Item -ItemType Directory -Path $AttemptRoot | Out-Null
        {Invoke-ModifiedNative $exe @('-NoProfile','-Command','exit 7') 'gpg-import'} | Should -Throw '*fixed-zero-candidate-marker*'
    }
}
Describe 'RELEASE A outer authority and original bytes' {
    It 'reports the named original failure rather than a Unicode variable lookup' {
        foreach ($record in @(
            @{ error='original-error-marker';timed_out=$false;exit=7 },
            @{ error=$null;timed_out=$false;exit=7 },
            @{ error=$null;timed_out=$true;exit=0 })) {
            $caught=$null
            try { Assert-ReleaseAResult $record '初始化边界' } catch { $caught=$_ }
            $caught | Should -Not -BeNullOrEmpty
            $caught.ToString() | Should -Match '^初始化边界失败：'
            $caught.ToString() | Should -Not -Match 'Name失败.*cannot be retrieved'
            $json=$caught.ToString().Substring('初始化边界失败：'.Length) | ConvertFrom-Json -AsHashtable
            $json.exit | Should -Be $record.exit
            $json.error | Should -Be $record.error
            $json.timed_out | Should -Be $record.timed_out
        }
        { Assert-ReleaseAResult @{error=$null;timed_out=$false;exit=0} '成功边界' } | Should -Not -Throw
    }
    It 'runs the fixed dependency-free bootstrap controls without entering the natural builder' {
        if (-not $IsWindows) { Set-ItResult -Skipped -Because '实际Windows PowerShell专用，不能用Mac代替'; return }
        $oldTemp=$env:RUNNER_TEMP
        try {
            $env:RUNNER_TEMP=Join-Path $TestDrive 'bootstrap-host'
            New-Item -ItemType Directory -Path $env:RUNNER_TEMP | Out-Null
            $repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
            $context=@{repo=$repo;transport=Join-Path $repo 'tools/windows-h264-recipe-transport.py';builderInvocations=0}
            $result=Invoke-ReleaseABootstrapDiagnostic $context -Source ('a'*40) -RunId '1234' -RunAttempt '1'
            $result.controls_passed | Should -BeTrue
            $result.formal_transport | Should -BeExactly 'incomplete-not-produced'
            $result.original_run_cause | Should -BeExactly 'UNKNOWN'
            $context.builderInvocations | Should -Be 0
            $result.cases.formatter.passed | Should -BeTrue
            $result.cases.'initialize-failure'.passed | Should -BeTrue
            $result.cases.'log-conflict'.raw_returned | Should -BeFalse
            $result.cases.'missing-exe'.record.exit | Should -BeNullOrEmpty
            Test-Path -LiteralPath (Join-Path $env:RUNNER_TEMP 'release-a.tar') | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $env:RUNNER_TEMP 'release-a.receipt.json') | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $env:RUNNER_TEMP 'release-a.verification.json') | Should -BeFalse
        }
        finally { $env:RUNNER_TEMP=$oldTemp }
    }
    It 'rejects default dispatch and unrelated events before executing anything' {
        (Test-ReleaseAAuthority 'workflow_dispatch' 'refs/heads/main' $false) | Should -BeFalse
        (Test-ReleaseAAuthority 'workflow_dispatch' 'refs/heads/main' $true) | Should -BeTrue
        (Test-ReleaseAAuthority 'push' 'refs/heads/main' $true) | Should -BeFalse
        (Test-ReleaseAAuthority 'push' 'refs/heads/codex/rdp-egfx-hevc-preference' $false) | Should -BeTrue
        (Test-ReleaseAAuthority 'pull_request' 'refs/heads/codex/rdp-egfx-hevc-preference' $true) | Should -BeFalse
    }
    It 'runs the actual fixed builder gateway once and retains failure' {
        $context = @{ repo = $TestDrive; builderInvocations = 0 }
        $script:calls = @()
        $runner = { param($Exe, $Arguments) $script:calls += ,@($Exe, $Arguments); return @{ exit = 7 } }
        $result = Invoke-ReleaseABuilderOnce $context $runner
        $result.exit | Should -Be 7
        { Invoke-ReleaseABuilderOnce $context $runner } | Should -Throw '*只能调用一次*'
        $script:calls.Count | Should -Be 1
        $script:calls[0][0] | Should -BeExactly 'pwsh'
        $script:calls[0][1] | Should -Be @('-NoProfile','-File',(Join-Path $TestDrive 'tools/build-ffmpeg-windows.ps1'),
            '-Configuration','Release','-Architecture','arm64','-WslDistribution','Ubuntu-24.04','-SourceRecipe','frd-h264dc-aarch64-v1')
    }
    It 'preserves the first actual error across later transport and capture failures' {
        $state = @{ first_error = $null; errors = @{ checkout=@(); setup=@(); builder=@(); capture=@(); transport=@() } }
        Add-ReleaseAError $state 'builder' 'original child exit7'
        Add-ReleaseAError $state 'transport' 'archive later failed'
        Add-ReleaseAError $state 'capture' 'drain later failed'
        $state.first_error.stage | Should -BeExactly 'builder'
        $state.first_error.message | Should -BeExactly 'original child exit7'
        $state.errors.transport | Should -Contain 'archive later failed'
    }
    It 'drains simultaneous large binary stdout and stderr without text conversion or deadlock' {
        $scriptPath = Join-Path $TestDrive 'binary-child.ps1'
        $source = @'
$out=[Console]::OpenStandardOutput();$err=[Console]::OpenStandardError()
$a=[byte[]](0,255,65,10);$b=[byte[]](0,254,66,13)
for($i=0;$i -lt 524288;$i++) { $out.Write($a,0,$a.Length);$err.Write($b,0,$b.Length) }
$out.Flush();$err.Flush();exit 7
'@
        Write-ReleaseALf $scriptPath $source
        $record = Invoke-ReleaseARaw -Exe (Get-Process -Id $PID).Path -Arguments @('-NoProfile','-File',$scriptPath) `
            -Directory $TestDrive -RecordDirectory (Join-Path $TestDrive 'binary-call') -TimeoutSeconds 30
        $record.exit | Should -Be 7
        $record.error | Should -BeNullOrEmpty
        $record.timed_out | Should -BeFalse
        $a = [IO.File]::ReadAllBytes((Join-Path $TestDrive 'binary-call/stdout.bin'))
        $b = [IO.File]::ReadAllBytes((Join-Path $TestDrive 'binary-call/stderr.bin'))
        $a.Length | Should -Be 2097152
        $b.Length | Should -Be 2097152
        $a[1] | Should -Be 255
        $b[1] | Should -Be 254
        [Convert]::ToBase64String($a) | Should -Not -Be ([Convert]::ToBase64String($b))
    }
    It 'records timeout independently and invokes Linux proof before its Windows-only kill' {
        $scriptPath = Join-Path $TestDrive 'waiting-child.ps1'
        Write-ReleaseALf $scriptPath 'Start-Sleep -Seconds 60'
        $script:proof = $false
        $record = Invoke-ReleaseARaw -Exe (Get-Process -Id $PID).Path -Arguments @('-NoProfile','-File',$scriptPath) `
            -Directory $TestDrive -RecordDirectory (Join-Path $TestDrive 'timeout-call') -TimeoutSeconds 1 `
            -BeforeWindowsKill { $script:proof = $true }
        $script:proof | Should -BeTrue
        $record.timed_out | Should -BeTrue
        { Assert-ReleaseAResult $record 'timeout' } | Should -Throw
    }
    It 'executes the actual modified native capture helper with original argv and separated raw bytes' {
        $tokens = $null; $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot '../build-ffmpeg-windows.ps1'),[ref]$tokens,[ref]$errors)
        $errors.Count | Should -Be 0
        $node = $ast.Find({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Invoke-ModifiedNative' }, $true)
        Invoke-Expression $node.Extent.Text
        $ModifiedRecipe = $true
        $AttemptRoot = Join-Path $TestDrive 'native-attempt'
        New-Item -ItemType Directory -Path $AttemptRoot | Out-Null
        $scriptPath = Join-Path $TestDrive 'native-byte-child.ps1'
        Write-ReleaseALf $scriptPath '[Console]::OpenStandardOutput().Write([byte[]](0,255,65),0,3);[Console]::OpenStandardError().Write([byte[]](0,254,66),0,3);exit 7'
        $exe = (Get-Process -Id $PID).Path
        $arguments = @('-NoProfile','-File',$scriptPath)
        $record = Invoke-ModifiedNative $exe $arguments 'gpg-import'
        $record.exit | Should -Be 7
        $record.capture_complete | Should -BeTrue
        $record.argv | Should -Be (@($exe) + $arguments)
        [IO.File]::ReadAllBytes($record.stdout)[1] | Should -Be 255
        [IO.File]::ReadAllBytes($record.stderr)[1] | Should -Be 254
        $record.executable.before_sha256 | Should -BeExactly $record.executable.after_sha256
        $record.executable.saved_sha256 | Should -BeExactly $record.executable.before_sha256
        $ModifiedRecipe = $false
        { Invoke-ModifiedNative $exe $arguments 'gpg-import' } | Should -Throw '*modified*'
    }
    It 'saves executable bytes with independent before and after identity' {
        $original = Join-Path $TestDrive 'inert-exe-fixture'
        $saved = Join-Path $TestDrive 'saved-exe.bin'
        [IO.File]::WriteAllBytes($original, [byte[]](0,255,12,34))
        $record = Save-ReleaseAExecutable 'fixed-fixture-name' $original $saved
        $record.invoked_name | Should -BeExactly 'fixed-fixture-name'
        $record.before_sha256 | Should -BeExactly $record.after_sha256
        $record.saved_sha256 | Should -BeExactly $record.before_sha256
        [IO.File]::ReadAllBytes($saved)[1] | Should -Be 255
    }
    It 'binds original pack bytes outside the tar and refuses overwriting that receipt' {
        $callRoot = Join-Path $TestDrive 'pack-call'
        New-Item -ItemType Directory -Path $callRoot | Out-Null
        Write-ReleaseAJson (Join-Path $callRoot 'argv.json') @{ argv = @('fixture'); cwd = $TestDrive }
        [IO.File]::WriteAllBytes((Join-Path $callRoot 'stdout.bin'), [byte[]](0,255,65))
        [IO.File]::WriteAllBytes((Join-Path $callRoot 'stderr.bin'), [byte[]](0,254,66))
        Write-ReleaseAJson (Join-Path $callRoot 'exit.json') @{ exit = 0; terminated = $true; timed_out = $false
            error = $null; capture_complete = $true; argv = @('fixture'); cwd = $TestDrive }
        $receipt = Join-Path $TestDrive 'receipt.json'
        Write-ReleaseAJson $receipt @{ tar_sha256 = 'independent-tar'; packing = $null }
        Complete-ReleaseAPackReceipt $receipt $callRoot
        $value = Read-ReleaseAJson $receipt
        $value.tar_sha256 | Should -BeExactly 'independent-tar'
        $value.packing.Keys.Count | Should -Be 4
        [Convert]::FromBase64String($value.packing['stdout.bin'].base64)[1] | Should -Be 255
        [Convert]::FromBase64String($value.packing['stderr.bin'].base64)[1] | Should -Be 254
        { Complete-ReleaseAPackReceipt $receipt $callRoot } | Should -Throw '*禁止覆盖*'
    }
    It 'does not kill an owned Windows handle already exited during its timeout callback' {
        $scriptPath = Join-Path $TestDrive 'callback-exit.ps1'
        Write-ReleaseALf $scriptPath 'Start-Sleep -Seconds 2; exit 0'
        $record = Invoke-ReleaseARaw -Exe (Get-Process -Id $PID).Path -Arguments @('-NoProfile','-File',$scriptPath) `
            -Directory $TestDrive -RecordDirectory (Join-Path $TestDrive 'callback-exit') -TimeoutSeconds 1 `
            -BeforeWindowsKill { Start-Sleep -Seconds 3 }
        $record.timed_out | Should -BeTrue
        $record.exit | Should -Be 0
        $record.error | Should -BeNullOrEmpty
        $record.capture_complete | Should -BeTrue
    }
    It 'allows callback-confirmed child to deliver its original exit within the existing cleanup budget' {
        $scriptPath = Join-Path $TestDrive 'callback-near-exit.ps1'
        Write-ReleaseALf $scriptPath 'Start-Sleep -Seconds 2; exit 124'
        $record = Invoke-ReleaseARaw -Exe (Get-Process -Id $PID).Path -Arguments @('-NoProfile','-File',$scriptPath) `
            -Directory $TestDrive -RecordDirectory (Join-Path $TestDrive 'callback-near-exit') -TimeoutSeconds 1 `
            -BeforeWindowsKill { }
        $record.timed_out | Should -BeTrue
        $record.exit | Should -Be 124
        $record.error | Should -BeNullOrEmpty
        $record.capture_complete | Should -BeTrue
        { Assert-ReleaseAResult $record 'natural builder' } | Should -Throw
    }
    It 'keeps ControlsOnly away from the future configuration HALT' {
        $repo = Join-Path $TestDrive 'control-repo'
        $root = Join-Path $TestDrive 'control-payload'
        New-Item -ItemType Directory -Path (Join-Path $repo 'tools'),(Join-Path $root 'setup') | Out-Null
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../build-ffmpeg-windows.ps1') -Destination (Join-Path $repo 'tools/build-ffmpeg-windows.ps1')
        Stop-ReleaseAScopes @{ repo = $repo; root = $root } -ControlsOnly
        Test-Path -LiteralPath (Join-Path $repo '.codex-target') | Should -BeFalse
    }
    It 'uses a shared remaining transport budget and refuses an exhausted one' {
        $context = @{ transportDeadline = [DateTime]::UtcNow.AddSeconds(3) }
        (Get-ReleaseATransportBudget $context) | Should -BeLessOrEqual 3
        $context.transportDeadline = [DateTime]::UtcNow.AddSeconds(-1)
        { Get-ReleaseATransportBudget $context } | Should -Throw '*预算耗尽*'
    }
    It 'routes the actual orchestration through append then one final reader gateway and preserves the pair' {
        $oldTemp=$env:RUNNER_TEMP; $oldSha=$env:GITHUB_SHA; $oldRun=$env:GITHUB_RUN_ID; $oldAttempt=$env:GITHUB_RUN_ATTEMPT
        try {
            $env:RUNNER_TEMP=Join-Path $TestDrive 'final-transport'
            $env:GITHUB_SHA='a'*40; $env:GITHUB_RUN_ID='1234'; $env:GITHUB_RUN_ATTEMPT='1'
            New-Item -ItemType Directory -Path $env:RUNNER_TEMP | Out-Null
            $tar=Join-Path $env:RUNNER_TEMP 'release-a.tar'; [IO.File]::WriteAllBytes($tar,[byte[]](0,255,1))
            $receipt=Join-Path $env:RUNNER_TEMP 'release-a.receipt.json'
            Write-ReleaseAJson $receipt @{ packing=$null }
            $pack=Join-Path $env:RUNNER_TEMP 'pack-record';New-Item -ItemType Directory -Path $pack | Out-Null
            Write-ReleaseAJson (Join-Path $pack 'argv.json') @{ argv=@('fixture-pack');cwd=$TestDrive }
            [IO.File]::WriteAllBytes((Join-Path $pack 'stdout.bin'),[byte[]](0,255));[IO.File]::WriteAllBytes((Join-Path $pack 'stderr.bin'),[byte[]](0,254))
            Write-ReleaseAJson (Join-Path $pack 'exit.json') @{ argv=@('fixture-pack');cwd=$TestDrive;exit=0;error=$null;timed_out=$false;terminated=$true;capture_complete=$true }
            $expectedRoot=Join-Path $env:RUNNER_TEMP 'payload'
            New-Item -ItemType Directory -Path (Join-Path $expectedRoot 'checkout-inputs') | Out-Null
            $transportPath=Join-Path $PSScriptRoot '../windows-h264-recipe-transport.py'
            Write-ReleaseAJson (Join-Path $expectedRoot 'checkout-inputs/blob-identities.json') @{ 'tools/windows-h264-recipe-transport.py'=(Get-FileHash -LiteralPath $transportPath -Algorithm SHA256).Hash.ToLowerInvariant() }
            $script:verificationCalls=0;$script:pairHashAfterAppend=$null
            Mock Invoke-ReleaseARaw {
                param($Exe,$Arguments,$Directory,$RecordDirectory,$TimeoutSeconds)
                $script:verificationCalls++
                (Read-ReleaseAJson $receipt).packing | Should -Not -BeNullOrEmpty
                $Arguments | Should -Contain 'verify-pair'
                $script:pairHashAfterAppend=(Get-FileHash -LiteralPath $receipt -Algorithm SHA256).Hash
                New-Item -ItemType Directory -Path $RecordDirectory | Out-Null
                Write-ReleaseAJson (Join-Path $RecordDirectory 'argv.json') @{ argv=@($Exe)+$Arguments;cwd=$Directory }
                Write-ReleaseAJson (Join-Path $RecordDirectory 'stdout.bin') @{ source=$env:GITHUB_SHA;run_id=$env:GITHUB_RUN_ID;run_attempt=$env:GITHUB_RUN_ATTEMPT
                    tar_size=(Get-Item -LiteralPath $tar).Length;tar_sha256=(Get-FileHash -LiteralPath $tar -Algorithm SHA256).Hash.ToLowerInvariant()
                    receipt_size=(Get-Item -LiteralPath $receipt).Length;receipt_sha256=$script:pairHashAfterAppend.ToLowerInvariant()
                    reader_sha256=(Get-FileHash -LiteralPath $transportPath -Algorithm SHA256).Hash.ToLowerInvariant()
                    expected_inputs_sha256=(Get-FileHash -LiteralPath (Join-Path $expectedRoot 'checkout-inputs/blob-identities.json') -Algorithm SHA256).Hash.ToLowerInvariant()
                    packing_complete=$true;verification_complete=$false;build_identity_passed=$false }
                [IO.File]::WriteAllBytes((Join-Path $RecordDirectory 'stderr.bin'),[byte[]](0,254))
                $result=@{ argv=@($Exe)+$Arguments;cwd=$Directory;exit=0;error=$null;timed_out=$false;terminated=$true;capture_complete=$true }
                Write-ReleaseAJson (Join-Path $RecordDirectory 'exit.json') $result
                return $result
            }
            $context=@{ repo=$TestDrive;root=$expectedRoot;transport=$transportPath;transportDeadline=[DateTime]::UtcNow.AddMinutes(10) }
            $paths=@{ wsl='/mnt/d/repo';evidence_wsl='/mnt/d/evidence' }
            Complete-ReleaseATransport $context $paths $receipt $pack
            $script:verificationCalls | Should -Be 1
            (Get-FileHash -LiteralPath $receipt -Algorithm SHA256).Hash | Should -BeExactly $script:pairHashAfterAppend
            $third=Read-ReleaseAJson (Join-Path $env:RUNNER_TEMP 'release-a.verification.json')
            $third.producer_error | Should -BeNullOrEmpty
            $third.inputs_before.receipt.sha256 | Should -BeExactly $third.inputs_after.receipt.sha256
            ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($third.files['stdout.bin'].base64)) | ConvertFrom-Json).source | Should -BeExactly $env:GITHUB_SHA
        }
        finally { $env:RUNNER_TEMP=$oldTemp; $env:GITHUB_SHA=$oldSha; $env:GITHUB_RUN_ID=$oldRun; $env:GITHUB_RUN_ATTEMPT=$oldAttempt }
    }
    It 'executes the unchanged upstream Invoke-Wsl branch with its original argv' {
        $tokens = $null; $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot '../build-ffmpeg-windows.ps1'),[ref]$tokens,[ref]$errors)
        $errors.Count | Should -Be 0
        $node = $ast.Find({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Invoke-Wsl' }, $true)
        Invoke-Expression $node.Extent.Text
        $ModifiedRecipe = $false
        $WslDistribution = 'fixture-distro'
        function wsl.exe { $script:actual = @($args); $global:LASTEXITCODE = 0; return 'fixture output' }
        Invoke-Wsl 'original cwd' 'original command' 'host fixture' | Should -BeExactly 'fixture output'
        $script:actual | Should -Be @('--distribution','fixture-distro','--cd','original cwd','--','bash','-lc','original command')
    }
}
