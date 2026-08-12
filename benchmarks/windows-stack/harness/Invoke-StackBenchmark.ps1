[CmdletBinding()]
param(
    [ValidateSet('Doctor', 'Build', 'Measure')]
    [string]$Mode = 'Doctor',

    [ValidateSet('all', 'wpf', 'tauri', 'slint', 'winui')]
    [string]$Candidate = 'all',

    [ValidateRange(1, 100)]
    [int]$ColdSamples = 20,

    [ValidateRange(1, 500)]
    [int]$HotSamples = 50
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Benchmark.Common.psm1') -Force
if (-not ('BenchmarkHotkeyInput' -as [type])) {
    Add-Type -TypeDefinition (Get-Content -Raw (Join-Path $PSScriptRoot 'HotkeyInput.cs'))
}

$root = Get-BenchmarkRoot
$candidateNames = if ($Candidate -eq 'all') { @('wpf', 'tauri', 'slint', 'winui') } else { @($Candidate) }

# The manifest makes missing release artifacts explicit blockers.
$manifests = @{
    wpf = @{
        Build = { & dotnet build (Join-Path $root 'wpf/QuickNote.StackBenchmark.Wpf.csproj') -c Release }
        Executable = Join-Path $root 'wpf/bin/Release/net481/QuickNote.StackBenchmark.Wpf.exe'
    }
    tauri = @{
        Build = { & (Join-Path $PSScriptRoot 'Build-RustCandidate.ps1') -Candidate tauri }
        Executable = Join-Path $root 'tauri/src-tauri/target/release/quicknote-stack-tauri.exe'
    }
    slint = @{
        Build = { & (Join-Path $PSScriptRoot 'Build-RustCandidate.ps1') -Candidate slint }
        Executable = Join-Path $root 'slint/target/release/quicknote-stack-slint.exe'
    }
    winui = @{
        Build = { & dotnet publish (Join-Path $root 'winui/QuickNote.StackBenchmark.WinUI.csproj') -c Release -r win-x64 --self-contained false }
        Executable = Join-Path $root 'winui/bin/Release/net10.0-windows10.0.19041.0/win-x64/publish/QuickNote.StackBenchmark.WinUI.exe'
    }
}

function Invoke-Doctor {
    foreach ($name in 'dotnet', 'node', 'npm', 'rustc', 'cargo') {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        $detail = if ($command) {
            if ($command.PSObject.Properties.Name -contains 'Source') { $command.Source } else { $command.Definition }
        } else { $null }
        [pscustomobject]@{ Kind = 'tool'; Name = $name; Available = [bool]$command; Detail = $detail }
    }

    foreach ($name in $candidateNames) {
        $manifest = $manifests[$name]
        [pscustomobject]@{
            Kind = 'candidate'
            Name = $name
            Available = Test-Path -LiteralPath $manifest.Executable
            Detail = $manifest.Executable
        }
    }
}

function Invoke-Build {
    foreach ($name in $candidateNames) {
        Write-Host "Building $name..."
        & $manifests[$name].Build
        if ($LASTEXITCODE -ne 0) {
            throw "Build failed for $name with exit code $LASTEXITCODE."
        }
    }
}

function Invoke-Measure {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $resultDirectory = Join-Path $root "results/$stamp"
    New-Item -ItemType Directory -Path $resultDirectory -Force | Out-Null
    Write-EnvironmentSnapshot -Path (Join-Path $resultDirectory 'environment.json')

    $fixturePath = (Resolve-Path (Join-Path $root 'fixtures/note.txt')).ProviderPath
    $runSummaries = @()

    foreach ($name in $candidateNames) {
        $manifest = $manifests[$name]
        if (-not (Test-Path -LiteralPath $manifest.Executable)) {
            $runSummaries += [pscustomobject]@{ candidate = $name; status = 'blocked'; reason = 'Release executable is missing.' }
            continue
        }

        Write-Host "Measuring $name..."
        # SQLite WAL needs a local filesystem; UNC-backed WSL paths are not valid DB locations.
        $dataDirectory = Join-Path $env:TEMP "quicknote-stack-benchmark/$stamp/$name"
        New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
        $environment = @{
            QUICKNOTE_BENCH_DATA_DIR = $dataDirectory
            QUICKNOTE_BENCH_FIXTURE = $fixturePath
        }
        $samples = [System.Collections.Generic.List[object]]::new()
        $process = $null
        $stageDirectory = Join-Path $env:TEMP "quicknote-stack-benchmark/$stamp/stage-$name"
        New-Item -ItemType Directory -Path $stageDirectory -Force | Out-Null
        $releaseDirectory = Split-Path -Parent $manifest.Executable
        Copy-Item -Path (Join-Path $releaseDirectory '*') -Destination $stageDirectory -Recurse -Force
        Get-ChildItem -LiteralPath $stageDirectory -Recurse -File | Unblock-File
        $executable = Join-Path $stageDirectory (Split-Path -Leaf $manifest.Executable)

        # Warm-ups validate process startup and IPC without entering the dataset.
        for ($warmup = 1; $warmup -le 2; $warmup++) {
            foreach ($key in $environment.Keys) { [Environment]::SetEnvironmentVariable($key, $environment[$key], 'Process') }
            $process = Start-Process -FilePath $executable -PassThru
            try { [void](Wait-CandidateReady -Candidate $name) } finally {
                try { [void](Invoke-CandidateCommand -Candidate $name -Command 'shutdown' -TimeoutMilliseconds 500) } catch { }
                if (-not $process.WaitForExit(2000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            }
        }

        for ($index = 1; $index -le $ColdSamples; $index++) {
            Start-Sleep -Seconds 5
            $endToEnd = [Diagnostics.Stopwatch]::StartNew()
            $process = Start-Process -FilePath $executable -PassThru
            $failure = $null
            $status = $null
            try {
                $status = Wait-CandidateReady -Candidate $name -TimeoutMilliseconds 5000
                $endToEnd.Stop()
                $milliseconds = $endToEnd.Elapsed.TotalMilliseconds
            } catch {
                $failure = $_.Exception.Message
                $milliseconds = $null
            }
            $samples.Add([pscustomobject]@{
                candidate=$name; metric='cold_start'; sample=$index; milliseconds=$milliseconds
                privateWorkingSetBytes=$null; privateBytes=$null; processCount=$null
                success=($null -eq $failure); error=$failure
            })
            try { [void](Invoke-CandidateCommand -Candidate $name -Command 'shutdown' -TimeoutMilliseconds 500) } catch { }
            if (-not $process.WaitForExit(2000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
        }

        $process = Start-Process -FilePath $executable -PassThru
        $status = Wait-CandidateReady -Candidate $name
        for ($index = 1; $index -le $HotSamples; $index++) {
            [void](Invoke-CandidateCommand -Candidate $name -Command 'hide')
            Start-Sleep -Milliseconds 100
            $previousSequence = [int64]$status.showSequence
            $endToEnd = [Diagnostics.Stopwatch]::StartNew()
            [BenchmarkHotkeyInput]::Send()
            $failure = $null
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            do {
                try { $status = Invoke-CandidateCommand -Candidate $name -Command 'status' -TimeoutMilliseconds 250 } catch { $status = $null }
                if ($status -and [int64]$status.showSequence -gt $previousSequence) { break }
                Start-Sleep -Milliseconds 1
            } while ($deadline.ElapsedMilliseconds -lt 5000)

            if (-not $status -or [int64]$status.showSequence -le $previousSequence) {
                $failure = 'No readiness acknowledgement within 5 seconds.'
                $milliseconds = $null
            } else {
                $endToEnd.Stop()
                $milliseconds = $endToEnd.Elapsed.TotalMilliseconds
            }
            $samples.Add([pscustomobject]@{
                candidate=$name; metric='hot_summon'; sample=$index; milliseconds=$milliseconds
                privateWorkingSetBytes=$null; privateBytes=$null; processCount=$null
                success=($null -eq $failure); error=$failure
            })
        }

        [void](Invoke-CandidateCommand -Candidate $name -Command 'hide')
        Start-Sleep -Seconds 30
        for ($index = 1; $index -le 10; $index++) {
            $tree = @(Get-ProcessTreeSnapshot -RootProcessId $process.Id)
            $samples.Add([pscustomobject]@{
                candidate=$name
                metric='idle_memory'
                sample=$index
                milliseconds=$null
                privateWorkingSetBytes=$(if (@($tree | Where-Object { $null -eq $_.PrivateWorkingSetBytes }).Count -eq 0) { [int64](($tree | Measure-Object PrivateWorkingSetBytes -Sum).Sum) } else { $null })
                privateBytes=[int64](($tree | Measure-Object PrivateBytes -Sum).Sum)
                processCount=$tree.Count
                success=$true
                error=$null
            })
            Start-Sleep -Seconds 1
        }

        try { [void](Invoke-CandidateCommand -Candidate $name -Command 'shutdown' -TimeoutMilliseconds 500) } catch { }
        if (-not $process.WaitForExit(2000)) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }

        $samplePath = Join-Path $resultDirectory "$name-samples.csv"
        $samples | Export-Csv -LiteralPath $samplePath -NoTypeInformation -Encoding UTF8
        $coldValues = @($samples | Where-Object { $_.metric -eq 'cold_start' -and $_.success } | ForEach-Object { [double]$_.milliseconds })
        $hotValues = @($samples | Where-Object { $_.metric -eq 'hot_summon' -and $_.success } | ForEach-Object { [double]$_.milliseconds })
        $memoryValues = @($samples | Where-Object { $_.metric -eq 'idle_memory' } | ForEach-Object { [double]$_.privateWorkingSetBytes })
        $releaseBytes = [int64]((Get-ChildItem -LiteralPath $releaseDirectory -Recurse -File | Measure-Object Length -Sum).Sum)
        $runSummaries += [pscustomobject]@{
            candidate=$name
            status='measured'
            coldP50Ms=(Get-NearestRankPercentile $coldValues 0.50)
            coldP95Ms=(Get-NearestRankPercentile $coldValues 0.95)
            hotP50Ms=(Get-NearestRankPercentile $hotValues 0.50)
            hotP95Ms=(Get-NearestRankPercentile $hotValues 0.95)
            idlePrivateWorkingSetP50Bytes=(Get-NearestRankPercentile $memoryValues 0.50)
            idlePrivateWorkingSetP95Bytes=(Get-NearestRankPercentile $memoryValues 0.95)
            releaseDirectoryBytes=$releaseBytes
            failureCount=@($samples | Where-Object { -not $_.success }).Count
        }
    }

    $run = [ordered]@{
        status = 'complete'
        interpretation = 'Exploratory only; reference lines are not pass/fail gates.'
        coldSamplesRequested = $ColdSamples
        hotSamplesRequested = $HotSamples
        candidates = $runSummaries
    }
    $run | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $resultDirectory 'summary.json') -Encoding UTF8
    $run | ConvertTo-Json -Depth 8
}

switch ($Mode) {
    'Doctor' { Invoke-Doctor | Format-Table -AutoSize }
    'Build' { Invoke-Build }
    'Measure' { Invoke-Measure }
}
