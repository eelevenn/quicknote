[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Benchmark.Common.psm1') -Force
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms

# Fail with a stable gate name so the recorded evidence is self-explanatory.
function Assert-UiGate {
    param([bool]$Condition, [string]$Name, [string]$Detail)
    if (-not $Condition) { throw "[$Name] $Detail" }
}
$root = Get-BenchmarkRoot
$applicationPath = Join-Path $env:LOCALAPPDATA 'Programs/QuickNoteSlintSpike/QuickNoteSlintSpike.exe'
$dataDirectory = Join-Path $env:TEMP 'quicknote-slint-ui-evidence'
$evidenceDirectory = Join-Path $root 'results/20260813-slint-spike'
Get-Process QuickNoteSlintSpike -ErrorAction SilentlyContinue | Stop-Process -Force
if (Test-Path -LiteralPath $dataDirectory) {
    Remove-Item -LiteralPath $dataDirectory -Recurse -Force
}
New-Item -ItemType Directory -Path $dataDirectory, $evidenceDirectory -Force | Out-Null
$env:QUICKNOTE_BENCH_DATA_DIR = $dataDirectory
$env:SLINT_SCALE_FACTOR = '1.5'
$process = Start-Process -FilePath $applicationPath -PassThru

try {
    [void](Wait-CandidateReady -Candidate slint -TimeoutMilliseconds 8000)
    $automationRoot = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    $descendants = $automationRoot.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition)
    $elements = @(
        for ($index = 0; $index -lt $descendants.Count; $index++) {
            $element = $descendants.Item($index)
            [pscustomobject]@{
                type = $element.Current.ControlType.ProgrammaticName
                name = $element.Current.Name
                help = $element.Current.HelpText
                keyboardFocusable = $element.Current.IsKeyboardFocusable
            }
        }
    )
    $editor = $descendants | Where-Object {
        $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Edit -and
        $_.Current.Name -eq '当前便签正文'
    } | Select-Object -First 1
    Assert-UiGate ($null -ne $editor) 'accessibility-tree' 'Named editor is missing from UI Automation.'
    Assert-UiGate $editor.Current.IsKeyboardFocusable 'keyboard-focus' 'Editor is not keyboard focusable.'
    foreach ($buttonName in @('安排测试提醒', '切换备用快捷键')) {
        Assert-UiGate ($elements.name -contains $buttonName) 'accessible-buttons' "Button '$buttonName' has no accessible name."
    }

    # Exercise direct UIA editing and keyboard select/paste/undo/redo paths.
    $valuePattern = [System.Windows.Automation.ValuePattern]$editor.GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern)
    $original = $valuePattern.Current.Value
    $editor.SetFocus()
    [System.Windows.Forms.Clipboard]::SetText('中文输入法验证 · IME caret')
    [System.Windows.Forms.SendKeys]::SendWait('^a')
    [System.Windows.Forms.SendKeys]::SendWait('^v')
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.Clipboard]::SetText(' · 粘贴验证')
    [System.Windows.Forms.SendKeys]::SendWait('^{END}')
    [System.Windows.Forms.SendKeys]::SendWait('^v')
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('^z')
    [System.Windows.Forms.SendKeys]::SendWait('^y')
    Start-Sleep -Milliseconds 500
    $finalText = ([System.Windows.Automation.ValuePattern]$editor.GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern)).Current.Value
    Assert-UiGate ($finalText -eq '中文输入法验证 · IME caret · 粘贴验证') 'text-editing' "Unexpected editor value: $finalText"

    $result = [ordered]@{
        capturedAt = (Get-Date).ToString('o')
        status = 'passed'
        scaleFactor = 1.5
        originalLength = $original.Length
        finalText = $finalText
        elements = $elements
    }
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $evidenceDirectory 'ui-accessibility.json') -Encoding UTF8
    $result | ConvertTo-Json -Depth 8
} finally {
    try { [void](Invoke-CandidateCommand -Candidate slint -Command 'shutdown' -TimeoutMilliseconds 250) } catch { }
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item Env:QUICKNOTE_BENCH_DATA_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:SLINT_SCALE_FACTOR -ErrorAction SilentlyContinue
}
