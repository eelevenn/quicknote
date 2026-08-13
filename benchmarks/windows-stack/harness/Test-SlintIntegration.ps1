[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Benchmark.Common.psm1') -Force

# Fail a gate with its stable name so the JSON evidence remains easy to audit.
function Assert-Gate {
    param([bool]$Condition, [string]$Name, [string]$Detail)
    if (-not $Condition) {
        throw "[$Name] $Detail"
    }
}

# Poll IPC state because native messages and protocol redirection are asynchronous.
function Wait-Status {
    param([scriptblock]$Predicate, [int]$TimeoutMilliseconds = 5000, [string]$Gate = 'status')
    $watch = [Diagnostics.Stopwatch]::StartNew()
    do {
        try {
            $status = Invoke-CandidateCommand -Candidate slint -Command 'status' -TimeoutMilliseconds 250
            if (& $Predicate $status) { return $status }
        } catch { }
        Start-Sleep -Milliseconds 25
    } while ($watch.ElapsedMilliseconds -lt $TimeoutMilliseconds)
    $last = if ($status) { $status | ConvertTo-Json -Compress -Depth 5 } else { '<unavailable>' }
    throw "[$Gate] Slint status predicate timed out after $TimeoutMilliseconds ms. Last status: $last"
}

$root = Get-BenchmarkRoot
$setup = (Resolve-Path (Join-Path $root 'slint/target/installer/QuickNoteSlintSpike-Setup.exe')).ProviderPath
$installedDirectory = Join-Path $env:LOCALAPPDATA 'Programs/QuickNoteSlintSpike'
$installedApplication = Join-Path $installedDirectory 'QuickNoteSlintSpike.exe'
$dataDirectory = Join-Path $env:TEMP 'quicknote-slint-integration-evidence'
$evidenceDirectory = Join-Path $root 'results/20260813-slint-spike'
$conflictSource = Join-Path $PSScriptRoot 'HotkeyConflict.cs'
$conflictExecutable = Join-Path $env:TEMP 'quicknote-hotkey-conflict.exe'
$application = $null
$blocker = $null
$gates = [System.Collections.Generic.List[object]]::new()

# Native child processes cannot inherit a WSL UNC working directory.
Set-Location $env:TEMP

try {
    Get-Process QuickNoteSlintSpike -ErrorAction SilentlyContinue | Stop-Process -Force
    $existingUninstaller = Join-Path $installedDirectory 'uninstall.cmd'
    if (Test-Path -LiteralPath $existingUninstaller) {
        & $existingUninstaller
        Start-Sleep -Seconds 2
    } elseif (Test-Path -LiteralPath $installedDirectory) {
        # Recover a partial prior uninstall of this spike's exact per-user directory.
        Remove-Item -LiteralPath $installedDirectory -Recurse -Force
    }
    if (Test-Path -LiteralPath $dataDirectory) {
        Remove-Item -LiteralPath $dataDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $dataDirectory, $evidenceDirectory -Force | Out-Null

    # IExpress returns after dispatch, so installed files and registry are the completion signal.
    & $setup
    $installWatch = [Diagnostics.Stopwatch]::StartNew()
    do {
        $registered = Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike'
        if ((Test-Path -LiteralPath $installedApplication) -and $registered) { break }
        Start-Sleep -Milliseconds 100
    } while ($installWatch.ElapsedMilliseconds -lt 15000)
    Assert-Gate (Test-Path -LiteralPath $installedApplication) 'installer' 'Installed executable is missing.'
    Assert-Gate $registered 'installer' 'Per-user uninstall registration is missing.'
    $installedBytes = [int64]((Get-ChildItem -LiteralPath $installedDirectory -Recurse -File | Measure-Object Length -Sum).Sum)
    $installerBytes = (Get-Item -LiteralPath $setup).Length
    Assert-Gate ($installerBytes -le 18MB) 'installer-size' "Installer is $installerBytes bytes."
    Assert-Gate ($installedBytes -le 45MB) 'installed-size' "Installed app is $installedBytes bytes."
    $gates.Add([pscustomobject]@{ name='installer'; status='passed'; installerBytes=$installerBytes; installedBytes=$installedBytes })

    $env:QUICKNOTE_BENCH_DATA_DIR = $dataDirectory
    $env:QUICKNOTE_BENCH_FIXTURE = (Resolve-Path (Join-Path $root 'fixtures/note.txt')).ProviderPath
    $application = Start-Process -FilePath $installedApplication -PassThru
    $initial = Wait-CandidateReady -Candidate slint -TimeoutMilliseconds 8000
    Assert-Gate $initial.hotkeyRegistered 'hotkey-initial' $initial.error

    # Reserve the alternate chord, detect the native conflict, then recover on the original chord.
    $compiler = "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
    & $compiler /nologo /out:$conflictExecutable $conflictSource
    Assert-Gate ($LASTEXITCODE -eq 0) 'hotkey-conflict' 'Could not compile the conflict helper.'
    $blocker = Start-Process -FilePath $conflictExecutable -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 500
    [void](Invoke-CandidateCommand -Candidate slint -Command 'rebind-hotkey' -Value 'Ctrl+Shift+Q')
    $conflict = Wait-Status -Gate 'hotkey-conflict' { param($value) -not $value.hotkeyRegistered }
    Assert-Gate ($conflict.error -match 'RegisterHotKey failed') 'hotkey-conflict' 'Native conflict was not exposed.'
    Stop-Process -Id $blocker.Id -Force
    $blocker = $null
    [void](Invoke-CandidateCommand -Candidate slint -Command 'rebind-hotkey' -Value 'Ctrl+Alt+Q')
    $recovered = Wait-Status -Gate 'hotkey-recovery' { param($value) $value.hotkeyRegistered -and $value.hotkeySpec -eq 'Ctrl+Alt+Q' }
    $gates.Add([pscustomobject]@{ name='hotkey-conflict-recovery'; status='passed'; error=$conflict.error })

    # Clear stale history so this run proves a new scheduled toast was delivered.
    $cleared = Invoke-CandidateCommand -Candidate slint -Command 'clear-history'
    Assert-Gate ($cleared.notificationHistoryCount -eq 0) 'scheduled-notification' 'Could not clear stale notification history.'

    # Windows accepts the native scheduled toast while the app is running.
    $scheduled = Invoke-CandidateCommand -Candidate slint -Command 'schedule-reminder' -Value '8'
    Assert-Gate ($scheduled.scheduledNotificationCount -ge 1) 'scheduled-notification' 'Windows did not retain the scheduled toast.'
    [void](Invoke-CandidateCommand -Candidate slint -Command 'shutdown')
    if (-not $application.WaitForExit(5000)) { throw '[scheduled-notification] App did not exit.' }
    $application = $null
    Start-Sleep -Seconds 10

    # A protocol activation cold-starts the installed app and routes to the reminder.
    $openUri = "quicknote-spike://open?note=1&reminder=1&delivery=$($scheduled.reminderDueAt)"
    $coldProcess = Start-Process -FilePath $installedApplication -ArgumentList $openUri -PassThru
    $application = $coldProcess
    $coldActivation = Wait-Status -Predicate { param($value) $value.lastActivation -eq $openUri } -TimeoutMilliseconds 8000 -Gate 'cold-activation'
    Assert-Gate ($coldActivation.notificationHistoryCount -ge 1) 'scheduled-notification' 'The due toast did not enter Windows notification history while the app was exited.'
    Assert-Gate ($coldActivation.activationCount -ge 1) 'cold-activation' 'Protocol activation was not routed.'
    $gates.Add([pscustomobject]@{ name='scheduled-toast-and-cold-activation'; status='passed'; historyCount=$coldActivation.notificationHistoryCount; uri=$openUri })

    # Duplicate snooze and archive launches must leave one deterministic domain result.
    $snoozeUri = "quicknote-spike://snooze?note=1&reminder=1&delivery=$($scheduled.reminderDueAt)"
    Start-Process -FilePath $installedApplication -ArgumentList $snoozeUri
    Start-Process -FilePath $installedApplication -ArgumentList $snoozeUri
    $snoozed = Wait-Status -Gate 'snooze-idempotence' { param($value) $value.reminderLastAction -eq 'snooze' }
    Assert-Gate ($snoozed.reminderStatus -eq 'scheduled') 'snooze-idempotence' 'Snooze did not reschedule the reminder.'
    Assert-Gate ($snoozed.scheduledNotificationCount -ge 1) 'snooze-idempotence' 'Snooze did not replace the Windows schedule.'
    $archiveUri = "quicknote-spike://archive?note=1&reminder=1&delivery=$($snoozed.reminderDueAt)"
    Start-Process -FilePath $installedApplication -ArgumentList $archiveUri
    Start-Process -FilePath $installedApplication -ArgumentList $archiveUri
    $archived = Wait-Status -Gate 'archive-idempotence' { param($value) $value.reminderLastAction -eq 'archive' }
    Assert-Gate ($archived.reminderStatus -eq 'cancelled') 'archive-idempotence' 'Archive did not cancel the reminder.'
    Assert-Gate ($archived.scheduledNotificationCount -eq 0) 'archive-idempotence' 'Archive left a pending Windows schedule.'
    $gates.Add([pscustomobject]@{ name='notification-action-idempotence'; status='passed'; activationCount=$archived.activationCount; scheduledAfterArchive=$archived.scheduledNotificationCount })

    # Drive the same WM_POWERBROADCAST path used by actual resume and verify catch-up once.
    $resumeSchedule = Invoke-CandidateCommand -Candidate slint -Command 'schedule-reminder' -Value '1'
    Start-Sleep -Seconds 2
    [void](Invoke-CandidateCommand -Candidate slint -Command 'simulate-resume')
    $firstResume = Wait-Status -Gate 'resume-first' { param($value) $null -ne $value.reminderCatchUpAt }
    $firstCatchUp = [int64]$firstResume.reminderCatchUpAt
    [void](Invoke-CandidateCommand -Candidate slint -Command 'simulate-resume')
    $secondResume = Wait-Status -Gate 'resume-second' { param($value) $value.resumeScanCount -ge ($firstResume.resumeScanCount + 1) }
    Assert-Gate ([int64]$secondResume.reminderCatchUpAt -eq $firstCatchUp) 'resume-idempotence' 'Duplicate resume changed catch-up state.'
    $gates.Add([pscustomobject]@{ name='resume-idempotence'; status='passed'; catchUpAt=$firstCatchUp; resumeScanCount=$secondResume.resumeScanCount })

    # Broadcast TaskbarCreated, which is the same Explorer-recovery signal handled by tray-icon.
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ExplorerRecoverySignal {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern uint RegisterWindowMessage(string value);
    [DllImport("user32.dll")] public static extern bool SendNotifyMessage(IntPtr window, uint message, UIntPtr wParam, IntPtr lParam);
}
'@
    $taskbarCreated = [ExplorerRecoverySignal]::RegisterWindowMessage('TaskbarCreated')
    [void][ExplorerRecoverySignal]::SendNotifyMessage([IntPtr]0xffff, $taskbarCreated, [UIntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 500
    $afterExplorer = Invoke-CandidateCommand -Candidate slint -Command 'status'
    Assert-Gate $afterExplorer.ok 'explorer-recovery' 'App stopped responding after TaskbarCreated.'
    $gates.Add([pscustomobject]@{ name='explorer-recovery-signal'; status='passed'; processId=$afterExplorer.pid })

    $result = [ordered]@{
        capturedAt = (Get-Date).ToString('o')
        status = 'passed'
        note = 'The resume and Explorer gates exercise their native Windows messages without suspending the workstation or terminating Explorer. Resume only marks missed-reminder state and never emits a catch-up toast, per ADR-0001.'
        gates = $gates
    }
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $evidenceDirectory 'integration.json') -Encoding UTF8
    $result | ConvertTo-Json -Depth 8
} finally {
    try { [void](Invoke-CandidateCommand -Candidate slint -Command 'shutdown' -TimeoutMilliseconds 250) } catch { }
    if ($application -and -not $application.HasExited) { Stop-Process -Id $application.Id -Force -ErrorAction SilentlyContinue }
    if ($blocker -and -not $blocker.HasExited) { Stop-Process -Id $blocker.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item Env:QUICKNOTE_BENCH_DATA_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:QUICKNOTE_BENCH_FIXTURE -ErrorAction SilentlyContinue
}
