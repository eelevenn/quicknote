[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('wpf', 'tauri', 'slint', 'winui')]
    [string]$Candidate,

    [ValidateRange(1, 20)]
    [int]$HotkeyIterations = 10,

    [ValidateRange(1, 20)]
    [int]$RestartIterations = 5,

    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Benchmark.Common.psm1') -Force
if (-not ('BenchmarkHotkeyInput' -as [type])) {
    Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'HotkeyInput.cs'))
}

$root = Get-BenchmarkRoot
$executables = @{
    wpf = Join-Path $root 'wpf/bin/Release/net481/QuickNote.StackBenchmark.Wpf.exe'
    tauri = Join-Path $root 'tauri/src-tauri/target/release/quicknote-stack-tauri.exe'
    slint = Join-Path $root 'slint/target/release/quicknote-stack-slint.exe'
    winui = Join-Path $root 'winui/bin/Release/net10.0-windows10.0.19041.0/win-x64/publish/QuickNote.StackBenchmark.WinUI.exe'
}

$sourceExecutable = $executables[$Candidate]
if (-not (Test-Path -LiteralPath $sourceExecutable)) {
    throw "Release executable is missing for $Candidate."
}

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$stageDirectory = Join-Path $env:TEMP "quicknote-stack-correctness/$stamp/stage-$Candidate"
$dataDirectory = Join-Path $env:TEMP "quicknote-stack-correctness/$stamp/data-$Candidate"
New-Item -ItemType Directory -Path $stageDirectory, $dataDirectory -Force | Out-Null
$releaseDirectory = Split-Path -Parent $sourceExecutable
Copy-Item -Path (Join-Path $releaseDirectory '*') -Destination $stageDirectory -Recurse -Force
Get-ChildItem $stageDirectory -Recurse -File | Unblock-File
$executable = Join-Path $stageDirectory (Split-Path -Leaf $sourceExecutable)
$env:QUICKNOTE_BENCH_DATA_DIR = $dataDirectory
$env:QUICKNOTE_BENCH_FIXTURE = (Resolve-Path (Join-Path $root 'fixtures/note.txt')).ProviderPath

$results = [System.Collections.Generic.List[object]]::new()
$process = Start-Process -FilePath $executable -PassThru
$status = Wait-CandidateReady -Candidate $Candidate

# A second launch must terminate without replacing the active instance.
$secondProcess = Start-Process -FilePath $executable -PassThru
$secondExited = $secondProcess.WaitForExit(2000)
if (-not $secondExited) {
    Stop-Process -Id $secondProcess.Id -Force -ErrorAction SilentlyContinue
}
$results.Add([pscustomobject]@{
    check = 'single_instance'
    iteration = 1
    success = $secondExited -and $secondProcess.ExitCode -eq 0
    detail = if ($secondExited) { "secondExitCode=$($secondProcess.ExitCode)" } else { 'second process remained alive' }
})

for ($index = 1; $index -le $HotkeyIterations; $index++) {
    $previousSequence = [int64]$status.showSequence
    [void](Invoke-CandidateCommand -Candidate $Candidate -Command 'hide')
    Start-Sleep -Milliseconds 100
    [BenchmarkHotkeyInput]::Send()
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        try { $status = Invoke-CandidateCommand -Candidate $Candidate -Command 'status' -TimeoutMilliseconds 250 } catch { $status = $null }
        if ($status -and [int64]$status.showSequence -gt $previousSequence) { break }
        Start-Sleep -Milliseconds 5
    } while ($timer.ElapsedMilliseconds -lt 5000)
    $results.Add([pscustomobject]@{
        check = 'hotkey_focus'
        iteration = $index
        success = [bool]($status -and [int64]$status.showSequence -gt $previousSequence)
        detail = if ($status) { "sequence=$($status.showSequence)" } else { 'no status response' }
    })
}

# Exercise the hide lifecycle immediately after a readiness mutation.
for ($index = 1; $index -le 3; $index++) {
    $response = Invoke-CandidateCommand -Candidate $Candidate -Command 'insert-sentinel' -Value "§$index"
    [void](Invoke-CandidateCommand -Candidate $Candidate -Command 'hide')
    $results.Add([pscustomobject]@{ check='hide_lifecycle'; iteration=$index; success=[bool]$response.ok; detail="sequence=$($response.showSequence)" })
}

try { [void](Invoke-CandidateCommand -Candidate $Candidate -Command 'shutdown' -TimeoutMilliseconds 500) } catch { }
if (-not $process.WaitForExit(2000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }

for ($index = 1; $index -le $RestartIterations; $index++) {
    $process = Start-Process -FilePath $executable -PassThru
    try {
        $status = Wait-CandidateReady -Candidate $Candidate
        $results.Add([pscustomobject]@{ check='restart_recovery'; iteration=$index; success=$true; detail="sequence=$($status.showSequence)" })
    } catch {
        $results.Add([pscustomobject]@{ check='restart_recovery'; iteration=$index; success=$false; detail=$_.Exception.Message })
    }
    try { [void](Invoke-CandidateCommand -Candidate $Candidate -Command 'shutdown' -TimeoutMilliseconds 500) } catch { }
    if (-not $process.WaitForExit(2000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
}

$failureCount = @($results | Where-Object { -not $_.success }).Count
$run = [ordered]@{
    candidate = $Candidate
    capturedAt = (Get-Date).ToString('o')
    interpretation = 'Lifecycle smoke only; this is not a content-level crash-recovery proof.'
    failureCount = $failureCount
    checks = $results
}
$json = $run | ConvertTo-Json -Depth 6
if (-not [string]::IsNullOrWhiteSpace($OutputPath)) {
    $parent = Split-Path -Parent $OutputPath
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    $json | Set-Content -LiteralPath $OutputPath -Encoding UTF8
}
$json
if ($failureCount -gt 0) {
    exit 1
}
