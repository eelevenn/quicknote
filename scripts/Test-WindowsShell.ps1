param(
    [Parameter(Mandatory = $true)]
    [string]$ExecutablePath,

    [ValidateRange(1, 100)]
    [int]$ColdRuns = 20,

    [ValidateRange(1, 200)]
    [int]$HotRuns = 50,

    [ValidateRange(1, 100)]
    [int]$MemorySamples = 10,

    [ValidateRange(1, 300)]
    [int]$IdleSeconds = 30,

    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ErrorView = 'DetailedView'
Add-Type -AssemblyName UIAutomationClient
. (Join-Path $PSScriptRoot 'QuickNote-VerificationEnvironment.ps1')
Assert-QuickNoteVerificationSafe

# 使用固定虚拟键发送 Ctrl+Alt+Q，确保测量经过真实的 RegisterHotKey 路径。
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class QuickNoteHotkeyInput
{
    private const uint KeyUp = 0x0002;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

    public static void PressCtrlAltQ()
    {
        keybd_event(0x11, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x51, 0, 0, UIntPtr.Zero);
        keybd_event(0x51, 0, KeyUp, UIntPtr.Zero);
        keybd_event(0x12, 0, KeyUp, UIntPtr.Zero);
        keybd_event(0x11, 0, KeyUp, UIntPtr.Zero);
    }

}
'@

# 固定使用绝对路径，避免测到另一个同名进程。
$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).ProviderPath
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\windows-shell'
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

# 数据库必须位于本地文件系统，不能放在 WSL UNC 工作区中。
$verificationRoot = Join-Path ([IO.Path]::GetTempPath()) ('quicknote-shell-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $verificationRoot | Out-Null
$processEnvironment = @{
    LOCALAPPDATA = $verificationRoot
    APPDATA = $verificationRoot
}

function Start-QuickNoteProcess {
    param([string[]]$Arguments)

    # GUI 仍按产品路径显示；WindowStyle 只避免额外创建控制台窗口。
    Start-Process `
        -FilePath $resolvedExecutable `
        -ArgumentList $Arguments `
        -Environment $processEnvironment `
        -PassThru `
        -WindowStyle Hidden
}

function Wait-ForEditableControl {
    param(
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutMilliseconds = 5000,
        [switch]$RequireKeyboardFocus
    )

    # 计时仍从进程启动前开始；先等消息循环就绪再读取 UIA。
    if (-not $Process.WaitForInputIdle($TimeoutMilliseconds)) {
        throw 'QuickNote did not reach an input-idle GUI state before the timeout.'
    }

    # UIA 中出现可见、可编辑控件才算快速记录真正可输入。
    $editCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Edit
    )
    $visibleCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsOffscreenProperty,
        $false
    )
    $condition = [System.Windows.Automation.AndCondition]::new($editCondition, $visibleCondition)
    if ($RequireKeyboardFocus) {
        $focusCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::HasKeyboardFocusProperty,
            $true
        )
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $focusCondition)
    }
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        if ($Process.HasExited) {
            throw "QuickNote exited before becoming editable with code $($Process.ExitCode)."
        }
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne [IntPtr]::Zero) {
            # 从 QuickNote 顶层窗口开始查询，避免每个样本扫描整个桌面 UIA 树。
            $window = [System.Windows.Automation.AutomationElement]::FromHandle($Process.MainWindowHandle)
            $editor = $window.FindFirst(
                [System.Windows.Automation.TreeScope]::Descendants,
                $condition
            )
            if ($null -ne $editor) {
                return $editor
            }
        }
        Start-Sleep -Milliseconds 10
    } while ([DateTime]::UtcNow -lt $deadline)

    throw 'QuickNote did not expose an editable UIA control before the timeout.'
}

function Wait-ForEditableControlHidden {
    param(
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutMilliseconds = 5000
    )

    # 下一轮热样本必须从快速记录窗口确实隐藏后的稳定后台状态开始。
    $processCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty,
        $Process.Id
    )
    $windowCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Window
    )
    $nameCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        'QuickNote · 快速记录'
    )
    $condition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.AndCondition]::new($processCondition, $windowCondition),
        $nameCondition
    )
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        if ($Process.HasExited) {
            throw "QuickNote exited while waiting for the quick-capture window to hide with code $($Process.ExitCode)."
        }
        # 顶层 Children 查询不会遍历整个桌面 UIA 树。
        $captureWindow = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [System.Windows.Automation.TreeScope]::Children,
            $condition
        )
        if ($null -eq $captureWindow) {
            return
        }
        Start-Sleep -Milliseconds 10
    } while ([DateTime]::UtcNow -lt $deadline)

    throw 'QuickNote quick-capture editor remained visible after the background reset request.'
}

function Get-QuickNoteProcessTree {
    param([int]$RootProcessId)

    # 单次抓取进程表后递归扩展 PID 集合，避免漏算应用启动的后代进程。
    $processes = @(Get-CimInstance -ClassName Win32_Process)
    $ids = [Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)
    do {
        $added = $false
        foreach ($candidate in $processes) {
            if ($ids.Contains([int]$candidate.ParentProcessId) -and $ids.Add([int]$candidate.ProcessId)) {
                $added = $true
            }
        }
    } while ($added)
    @($processes | Where-Object { $ids.Contains([int]$_.ProcessId) })
}

function Stop-VerificationProcess {
    param([System.Diagnostics.Process]$Process)

    if (-not $Process.HasExited) {
        # 只终止本脚本刚启动且已捕获 PID 的验证进程。
        $Process.Kill()
        $Process.WaitForExit()
    }
    $Process.Dispose()
}

# UIA 真实写入后等待“已自动保存”，避免把空库的后台内存当成日常使用结果。
function Save-VerificationNote {
    param([System.Diagnostics.Process]$Process, [string]$Body)

    $editor = Wait-ForEditableControl -Process $Process
    $editor.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Body)
    $savedCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, '已自动保存'
    )
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    do {
        if ($Process.HasExited) { throw '性能验收正文保存前应用已退出。' }
        $Process.Refresh()
        $window = [System.Windows.Automation.AutomationElement]::FromHandle($Process.MainWindowHandle)
        $saved = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $savedCondition)
        if ($saved -and $editor.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -eq $Body) { return }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw '性能验收正文未完成真实自动保存。'
}

# 复用产品的单实例激活隐藏窗口；次实例也在超时路径中回收。
function Hide-VerificationCapture {
    param([System.Diagnostics.Process]$Process)

    $secondary = Start-QuickNoteProcess -Arguments @('--startup')
    try {
        if (-not $secondary.WaitForExit(5000) -or $secondary.ExitCode -ne 0) {
            throw 'QuickNote 次实例未成功完成后台激活。'
        }
    }
    finally { Stop-VerificationProcess -Process $secondary }
    Wait-ForEditableControlHidden -Process $Process
}

function Get-Percentile95 {
    param([double[]]$Values)

    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($sorted.Count * 0.95) - 1)
    $sorted[$index]
}

$coldSamples = @()
$hotSamples = @()
$memoryResults = @()
$registryCopies = @()
$result = $null
$success = $false
$failure = $null
$outputPath = Join-Path $OutputDirectory ((Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
try {
    $registryCopies = @(Save-QuickNoteVerificationRegistry -Directory $verificationRoot)
    # 预热建立真实便签，后续冷启动读取持久正文，后台采样也会再写入一次。
    $warmup = Start-QuickNoteProcess -Arguments @('--quick-capture')
    try { Save-VerificationNote -Process $warmup -Body '性能验收：已持久保存的中文便签' }
    finally { Stop-VerificationProcess -Process $warmup }

    for ($run = 1; $run -le $ColdRuns; $run++) {
        $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        $process = Start-QuickNoteProcess -Arguments @('--quick-capture')
        try {
            Wait-ForEditableControl -Process $process | Out-Null
            $stopwatch.Stop()
            $coldSamples += [ordered]@{
                run = $run
                ready_ms = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            }
        }
        finally {
            Stop-VerificationProcess -Process $process
        }
    }

    # 热启动只复用一个已稳定的后台进程，并通过真实全局快捷键切换窗口。
    $hotProcess = Start-QuickNoteProcess -Arguments @('--startup')
    try {
        if (-not $hotProcess.WaitForInputIdle(5000)) {
            throw 'QuickNote background process did not reach input-idle before the hot-start benchmark.'
        }
        Start-Sleep -Milliseconds 500
        for ($run = 1; $run -le $HotRuns; $run++) {
            $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
            [QuickNoteHotkeyInput]::PressCtrlAltQ()
            Wait-ForEditableControl -Process $hotProcess -RequireKeyboardFocus | Out-Null
            $stopwatch.Stop()
            $hotSamples += [ordered]@{
                run = $run
                ready_ms = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            }

            # 复位不计入热启动时间，下一轮必须从隐藏窗口开始。
            Hide-VerificationCapture -Process $hotProcess
        }
    }
    finally {
        Stop-VerificationProcess -Process $hotProcess
    }

    # 编辑、真实自动保存并收起后采集完整后代进程树。
    $memoryProcess = Start-QuickNoteProcess -Arguments @('--quick-capture')
    try {
        Save-VerificationNote -Process $memoryProcess -Body '性能验收：保存正文后收起并测量后台内存'
        Hide-VerificationCapture -Process $memoryProcess
        Start-Sleep -Seconds $IdleSeconds
        for ($sample = 1; $sample -le $MemorySamples; $sample++) {
            $tree = @(Get-QuickNoteProcessTree -RootProcessId $memoryProcess.Id)
            $perProcess = @()
            foreach ($treeProcess in $tree) {
                # WorkingSetPrivate 对应验收口径中的 Private Working Set。
                $performance = Get-CimInstance `
                    -ClassName Win32_PerfFormattedData_PerfProc_Process `
                    -Filter "IDProcess = $($treeProcess.ProcessId)"
                if ($null -eq $performance) {
                    throw "Unable to read private working set for PID $($treeProcess.ProcessId)."
                }
                $perProcess += [ordered]@{
                    pid = [int]$treeProcess.ProcessId
                    parent_pid = [int]$treeProcess.ParentProcessId
                    name = [string]$treeProcess.Name
                    private_working_set_bytes = [int64]$performance.WorkingSetPrivate
                }
            }
            $memoryResults += [ordered]@{
                sample = $sample
                private_working_set_bytes = [int64](($perProcess | ForEach-Object { $_.private_working_set_bytes } | Measure-Object -Sum).Sum)
                process_tree = $perProcess
            }
            Start-Sleep -Milliseconds 250
        }
    }
    finally {
        Stop-VerificationProcess -Process $memoryProcess
    }

    $coldValues = [double[]]@($coldSamples | ForEach-Object { $_.ready_ms })
    $hotValues = [double[]]@($hotSamples | ForEach-Object { $_.ready_ms })
    $memoryValues = [double[]]@($memoryResults | ForEach-Object { $_.private_working_set_bytes })
    $result = [ordered]@{
        measured_at = [DateTime]::UtcNow.ToString('o')
        executable = $resolvedExecutable
        executable_sha256 = (Get-FileHash -LiteralPath $resolvedExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
        executable_bytes = (Get-Item -LiteralPath $resolvedExecutable).Length
        data_directory = $verificationRoot
        memory_scenario = '已真实自动保存中文正文并收起快速记录后采样'
        environment = [ordered]@{
            os = [System.Environment]::OSVersion.VersionString
            logical_processors = [System.Environment]::ProcessorCount
        }
        summary = [ordered]@{
            cold_start_p95_ms = [Math]::Round((Get-Percentile95 -Values $coldValues), 3)
            hot_start_p95_ms = [Math]::Round((Get-Percentile95 -Values $hotValues), 3)
            idle_private_working_set_p95_bytes = [int64](Get-Percentile95 -Values $memoryValues)
            cold_start_gate_ms = 900
            hot_start_gate_ms = 90
            idle_private_working_set_gate_bytes = 60MB
            installed_app_gate_bytes = 45MB
        }
        cold_samples = $coldSamples
        hot_samples = $hotSamples
        memory_samples = $memoryResults
    }

    # 始终保留测量证据，再让任一硬预算超限产生非零退出。
    $gateFailures = @()
    if ($result.summary.cold_start_p95_ms -gt $result.summary.cold_start_gate_ms) {
        $gateFailures += "cold start P95 $($result.summary.cold_start_p95_ms) ms > $($result.summary.cold_start_gate_ms) ms"
    }
    if ($result.summary.hot_start_p95_ms -gt $result.summary.hot_start_gate_ms) {
        $gateFailures += "hot start P95 $($result.summary.hot_start_p95_ms) ms > $($result.summary.hot_start_gate_ms) ms"
    }
    if ($result.summary.idle_private_working_set_p95_bytes -gt $result.summary.idle_private_working_set_gate_bytes) {
        $gateFailures += "idle private working set P95 $($result.summary.idle_private_working_set_p95_bytes) bytes > $($result.summary.idle_private_working_set_gate_bytes) bytes"
    }
    # 该脚本先约束 Release 可执行文件；安装目录总量由 MSI 生命周期验收另行记录。
    if ($result.executable_bytes -gt $result.summary.installed_app_gate_bytes) {
        $gateFailures += "application body $($result.executable_bytes) bytes > $($result.summary.installed_app_gate_bytes) bytes"
    }
    if ($gateFailures.Count -gt 0) {
        throw "Windows shell hard budget failed: $($gateFailures -join '; ')"
    }
    $success = $true
}
catch {
    $failure = $_.Exception.Message
    throw
}
finally {
    # 无论采样还是预算检查失败，都恢复注册并保存已有样本和隔离数据证据。
    $completion = Complete-QuickNoteVerification -Directory $verificationRoot -OutputDirectory $OutputDirectory -RegistryCopies $registryCopies -Succeeded $success
    if ($completion.Error) {
        $success = $false
        $failure = (@($failure, $completion.Error) | Where-Object { $_ }) -join '；'
    }
    if ($null -eq $result) {
        $result = [ordered]@{
            measured_at = [DateTime]::UtcNow.ToString('o')
            executable = $resolvedExecutable
            executable_sha256 = (Get-FileHash -LiteralPath $resolvedExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
            data_directory = $verificationRoot
            cold_samples = $coldSamples
            hot_samples = $hotSamples
            memory_samples = $memoryResults
        }
    }
    $result['success'] = $success
    $result['failure'] = $failure
    $result['data_directory_retained'] = $completion.Retained
    $result['failure_evidence'] = $completion.EvidenceDirectory
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $result | ConvertTo-Json -Depth 8
    Write-Output "Result: $outputPath"
    if ($completion.Error) { throw $completion.Error }
}
