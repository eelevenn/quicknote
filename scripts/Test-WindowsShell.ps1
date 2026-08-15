param(
    [Parameter(Mandatory = $true)]
    [string]$ExecutablePath,

    [ValidateRange(1, 100)]
    [int]$ColdRuns = 20,

    [ValidateRange(1, 100)]
    [int]$MemorySamples = 10,

    [ValidateRange(1, 300)]
    [int]$IdleSeconds = 30,

    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$ErrorView = 'DetailedView'
Add-Type -AssemblyName UIAutomationClient

# 固定使用绝对路径，避免测到另一个同名进程。
$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).Path
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\windows-shell'
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

# 数据库必须位于本地文件系统，不能放在 WSL UNC 工作区中。
$verificationRoot = Join-Path ([IO.Path]::GetTempPath()) ('quicknote-shell-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $verificationRoot | Out-Null
$processEnvironment = @{ LOCALAPPDATA = $verificationRoot }

function Start-QuickNoteProcess {
    param([string[]]$Arguments)

    # 隐藏测试窗口，避免自动测量打断当前桌面会话。
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
        [int]$TimeoutMilliseconds = 5000
    )

    # UIA 中出现可编辑控件才算快速记录真正可输入。
    $processCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty,
        $Process.Id
    )
    $editCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Edit
    )
    $condition = [System.Windows.Automation.AndCondition]::new(
        $processCondition,
        $editCondition
    )
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        if ($Process.HasExited) {
            throw "QuickNote exited before becoming editable with code $($Process.ExitCode)."
        }
        $editor = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        if ($null -ne $editor) {
            return
        }
        Start-Sleep -Milliseconds 10
    } while ([DateTime]::UtcNow -lt $deadline)

    throw 'QuickNote did not expose an editable UIA control before the timeout.'
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

function Get-Percentile95 {
    param([double[]]$Values)

    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($sorted.Count * 0.95) - 1)
    $sorted[$index]
}

# 预热只负责建立 schema；正式样本仍是新的冷进程。
$warmup = Start-QuickNoteProcess -Arguments @('--quick-capture')
try {
    Wait-ForEditableControl -Process $warmup
}
finally {
    Stop-VerificationProcess -Process $warmup
}

$coldSamples = @()
for ($run = 1; $run -le $ColdRuns; $run++) {
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = Start-QuickNoteProcess -Arguments @('--quick-capture')
    try {
        Wait-ForEditableControl -Process $process
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

# 隐藏窗口稳定后采集完整进程；当前 Slint 壳没有子进程。
$memoryProcess = Start-QuickNoteProcess -Arguments @('--quick-capture')
$memoryResults = @()
try {
    Wait-ForEditableControl -Process $memoryProcess
    Start-Sleep -Seconds $IdleSeconds
    for ($sample = 1; $sample -le $MemorySamples; $sample++) {
        # 性能计数器的 WorkingSetPrivate 对应验收口径中的 Private Working Set。
        $performance = Get-CimInstance `
            -ClassName Win32_PerfFormattedData_PerfProc_Process `
            -Filter "IDProcess = $($memoryProcess.Id)"
        if ($null -eq $performance) {
            throw 'Unable to read the QuickNote private working set.'
        }
        $memoryResults += [ordered]@{
            sample = $sample
            private_working_set_bytes = [int64]$performance.WorkingSetPrivate
        }
        Start-Sleep -Milliseconds 250
    }
}
finally {
    Stop-VerificationProcess -Process $memoryProcess
}

$coldValues = [double[]]@($coldSamples | ForEach-Object { $_.ready_ms })
$memoryValues = [double[]]@($memoryResults | ForEach-Object { $_.private_working_set_bytes })
$result = [ordered]@{
    measured_at = [DateTime]::UtcNow.ToString('o')
    executable = $resolvedExecutable
    executable_bytes = (Get-Item -LiteralPath $resolvedExecutable).Length
    data_directory = $verificationRoot
    environment = [ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        logical_processors = [System.Environment]::ProcessorCount
    }
    summary = [ordered]@{
        cold_start_p95_ms = [Math]::Round((Get-Percentile95 -Values $coldValues), 3)
        idle_private_working_set_p95_bytes = [int64](Get-Percentile95 -Values $memoryValues)
        cold_start_gate_ms = 900
        idle_private_working_set_gate_bytes = 60MB
        installed_app_gate_bytes = 45MB
    }
    cold_samples = $coldSamples
    memory_samples = $memoryResults
}

$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$outputPath = Join-Path $OutputDirectory "$timestamp.json"
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
$result | ConvertTo-Json -Depth 8
Write-Output "Result: $outputPath"
