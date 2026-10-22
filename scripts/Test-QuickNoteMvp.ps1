[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath,
    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
. (Join-Path $PSScriptRoot 'QuickNote-VerificationEnvironment.ps1')

# 文件、注册表和计划提醒均通过保护后，才能启动使用空库的验证进程。
Assert-QuickNoteVerificationSafe
$executable = (Resolve-Path -LiteralPath $ExecutablePath).ProviderPath
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'artifacts\mvp-smoke' }
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$verificationRoot = Join-Path ([IO.Path]::GetTempPath()) ('quicknote-mvp-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $verificationRoot | Out-Null
$environment = @{ LOCALAPPDATA = $verificationRoot; APPDATA = $verificationRoot; RUST_BACKTRACE = '1' }
$process = $null
$checks = [Collections.Generic.List[string]]::new()
$success = $false
$failure = $null
$registryCopies = @()

# 使用 UIA 值修改和控件动作，兼容没有前台输入权限的自动化会话。

function Start-TestProcess {
    param([string[]]$Arguments = @('--quick-capture'))
    # 每个进程独立保留异常诊断，避免 GUI 子系统崩溃时只有退出码可查。
    $errorLog = [IO.Path]::GetFullPath((Join-Path $OutputDirectory ('process-' + [guid]::NewGuid().ToString('N') + '.stderr.log')))
    Start-Process -FilePath $executable -ArgumentList $Arguments -Environment $environment -WindowStyle Hidden -RedirectStandardError $errorLog -PassThru
}

function Find-Control {
    param([string]$Name, [switch]$Prefix)
    $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, $process.Id)
    $windows = [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition)
    foreach ($window in $windows) {
        $items = $window.FindAll([Windows.Automation.TreeScope]::Subtree, [Windows.Automation.Condition]::TrueCondition)
        foreach ($item in $items) {
            if (-not $item.Current.IsOffscreen -and (($Prefix -and $item.Current.Name.StartsWith($Name)) -or $item.Current.Name -eq $Name)) { return $item }
        }
    }
    return $null
}

function Wait-Control {
    param([string]$Name, [switch]$Prefix)
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    do {
        if ($process.HasExited) { throw "应用提前退出：$($process.ExitCode)" }
        $control = Find-Control -Name $Name -Prefix:$Prefix
        if ($control) { return $control }
        Start-Sleep -Milliseconds 80
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "等待控件超时：$Name"
}

function Invoke-Control {
    param([string]$Name, [switch]$Prefix)
    $control = Wait-Control -Name $Name -Prefix:$Prefix
    $control.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
}

function Activate-Main {
    $secondary = Start-TestProcess -Arguments @('--main')
    try {
        if (-not $secondary.WaitForExit(5000) -or $secondary.ExitCode -ne 0) { throw '单实例激活失败。' }
    }
    finally {
        if (-not $secondary.HasExited) { $secondary.Kill(); $secondary.WaitForExit() }
        $secondary.Dispose()
    }
    Wait-Control -Name '主页便签 Markdown 原文' | Out-Null
}

function Assert-Editor {
    param([string]$Name, [string]$Expected)
    $editor = Wait-Control -Name $Name
    $actual = $editor.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value
    if ($actual -ne $Expected) { throw "编辑器正文不一致：$actual" }
    return $editor
}

try {
    # 原注册备份失败时尚未启动应用，因此不会把“读取失败”误当成原本不存在。
    $registryCopies = @(Save-QuickNoteVerificationRegistry -Directory $verificationRoot)
    $body = 'MVP验收：中文记录与恢复'
    $process = Start-TestProcess
    $editor = Wait-Control -Name '快速记录正文'
    # UIA 的 SetValue 直接送入编辑控件，不依赖当前桌面允许前台输入。
    $editor.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue($body)
    Wait-Control -Name '已自动保存' | Out-Null
    Assert-Editor -Name '快速记录正文' -Expected $body | Out-Null
    $checks.Add('中文输入和真实自动保存')
    Invoke-Control -Name '关闭'
    Start-Sleep -Milliseconds 400
    if ($process.HasExited -or (Find-Control -Name '快速记录正文')) { throw '关闭快速记录没有保留后台并隐藏窗口。' }
    $checks.Add('快速记录收起并后台常驻（Esc 另由共享 UI 测试覆盖）')

    Activate-Main
    Assert-Editor -Name '主页便签 Markdown 原文' -Expected $body | Out-Null
    $search = Wait-Control -Name '搜索活跃与归档便签'
    $search.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('MVP验收')
    Invoke-Control -Name '搜索'
    Wait-Control -Name '搜索结果' | Out-Null
    Invoke-Control -Name $body
    Assert-Editor -Name '主页便签 Markdown 原文' -Expected $body | Out-Null
    # 系统关闭按钮必须隐藏主页而不是终止后台。
    $process.Refresh()
    $mainWindow = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    $mainWindow.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern).Close()
    Start-Sleep -Milliseconds 350
    if ($process.HasExited -or (Find-Control -Name '主页便签 Markdown 原文')) { throw '关闭主页没有保持后台常驻。' }
    Activate-Main
    $checks.Add('单实例切换主页和全文搜索')
    $checks.Add('系统关闭按钮隐藏主页并保留后台')

    Invoke-Control -Name '设置'
    Invoke-Control -Name '新建'
    $editor = Assert-Editor -Name '主页便签 Markdown 原文' -Expected ''
    $editor.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue('第二张便签')
    Wait-Control -Name '已自动保存' | Out-Null
    Invoke-Control -Name '归档'
    Invoke-Control -Name '归档 1'
    Invoke-Control -Name '取消归档'
    Wait-Control -Name '归档为空' | Out-Null
    $checks.Add('从设置新建和归档/取消归档')

    Invoke-Control -Name '设置'
    Invoke-Control -Name '立即备份'
    Wait-Control -Name '最近自动备份：' -Prefix | Out-Null
    Invoke-Control -Name '导出完整 JSON'
    $dialog = Wait-Control -Name '导出便签' -Prefix
    # 系统对话框的文件名输入与保存按钮使用稳定控件 ID，避免全局键盘注入。
    $filenameCondition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, '1001')
    $filename = $dialog.FindFirst([Windows.Automation.TreeScope]::Descendants, $filenameCondition)
    if (-not $filename) { throw '系统保存对话框未提供文件名输入。' }
    $exportPath = Join-Path $verificationRoot 'export.json'
    $filename.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).SetValue($exportPath)
    # 文件列表项也可能使用 ID 1；同时限定按钮角色，避免误打开同 ID 的文件。
    $saveCondition = [Windows.Automation.AndCondition]::new(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, '1'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Button)
    )
    $dialog.FindFirst([Windows.Automation.TreeScope]::Descendants, $saveCondition).GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Control -Name '完整 JSON 已原子导出到' -Prefix | Out-Null
    $export = Get-Content -LiteralPath $exportPath -Raw | ConvertFrom-Json
    if ($export.notes.Count -ne 2) { throw 'JSON 导出缺少便签。' }
    $checks.Add('系统保存对话框、完整 JSON 导出和立即备份')
    Invoke-Control -Name '退出'
    if (-not $process.WaitForExit(5000)) { throw '显式退出未停止后台。' }

    # 写坏的仅是本次随机临时目录里的数据库，验证恢复 UI 不接触真实数据。
    [IO.File]::WriteAllText((Join-Path $verificationRoot 'QuickNote\quicknote.db'), 'invalid sqlite')
    $process = Start-TestProcess
    Wait-Control -Name '需要恢复本地数据' | Out-Null
    Invoke-Control -Name '恢复最近备份：' -Prefix
    Wait-Control -Name '快速记录正文' | Out-Null
    Assert-Editor -Name '快速记录正文' -Expected $body | Out-Null
    Activate-Main
    Invoke-Control -Name '退出'
    if (-not $process.WaitForExit(5000)) { throw '恢复后的退出失败。' }
    $checks.Add('损坏隔离、恢复选择和恢复后重新进入应用')
    $success = $true
}
catch {
    $failure = $_.Exception.Message
    # 失败时保留本次应用的 UIA 树，定位输入、焦点或布局问题。
    if ($process -and -not $process.HasExited) {
        $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, $process.Id)
        $windows = [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition)
        $diagnostics = foreach ($window in $windows) {
            foreach ($item in $window.FindAll([Windows.Automation.TreeScope]::Subtree, [Windows.Automation.Condition]::TrueCondition)) {
                [ordered]@{ name = $item.Current.Name; automation_id = $item.Current.AutomationId; type = $item.Current.ControlType.ProgrammaticName; focused = $item.Current.HasKeyboardFocus; offscreen = $item.Current.IsOffscreen }
            }
        }
        $diagnostics | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'failure-uia.json') -Encoding utf8
    }
    throw
}
finally {
    if ($process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $completion = Complete-QuickNoteVerification -Directory $verificationRoot -OutputDirectory $OutputDirectory -RegistryCopies $registryCopies -Succeeded $success
    if ($completion.Error) {
        $success = $false
        $failure = (@($failure, $completion.Error) | Where-Object { $_ }) -join '；'
    }
    # 失败也写入结果，避免仅有部分通过项的报告被当作整条旅程成功。
    [ordered]@{
        checked_at = [DateTime]::UtcNow.ToString('o')
        executable = $executable
        executable_sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
        success = $success
        failure = $failure
        passed = @($checks)
        isolated_data = $verificationRoot
        isolated_data_retained = $completion.Retained
        failure_evidence = $completion.EvidenceDirectory
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'journeys.json') -Encoding utf8
    if ($completion.Error) { throw $completion.Error }
}
$checks | ForEach-Object { Write-Output "PASS $_" }
