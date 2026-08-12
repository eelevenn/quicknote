Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Discover the standalone Rust MSI used by this benchmark host.
$standaloneRustBin = 'C:\Program Files\Rust stable MSVC 1.97\bin'
if (Test-Path -LiteralPath $standaloneRustBin) {
    $env:Path = "$standaloneRustBin;$env:Path"
}

# Return the benchmark root without relying on the caller's current directory.
function Get-BenchmarkRoot {
    # ProviderPath avoids the PowerShell provider prefix on UNC workspaces.
    return (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
}

# Calculate a nearest-rank percentile; return null for an empty sample.
function Get-NearestRankPercentile {
    param(
        [double[]]$Values,
        [ValidateRange(0.0, 1.0)][double]$Percentile
    )

    if (-not $Values -or $Values.Count -eq 0) {
        return $null
    }

    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($Percentile * $sorted.Count) - 1)
    return [double]$sorted[$index]
}

# Find descendants recursively so WebView2 child processes enter the totals.
function Get-ProcessTreeSnapshot {
    param([Parameter(Mandatory)][int]$RootProcessId)

    $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name)
    $ids = [System.Collections.Generic.HashSet[int]]::new()
    [void]$ids.Add($RootProcessId)

    do {
        $changed = $false
        foreach ($process in $all) {
            if ($ids.Contains([int]$process.ParentProcessId) -and -not $ids.Contains([int]$process.ProcessId)) {
                [void]$ids.Add([int]$process.ProcessId)
                $changed = $true
            }
        }
    } while ($changed)

    $snapshots = foreach ($id in $ids) {
        $process = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($process) {
            $privateWorkingSet = $null
            try {
                $counter = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess = $id" -ErrorAction Stop
                if ($counter) { $privateWorkingSet = [int64]$counter.WorkingSetPrivate }
            } catch {
                # The counter instance naming differs for duplicate process names; null is honest.
            }
            [pscustomobject]@{
                ProcessId = $process.Id
                Name = $process.ProcessName
                PrivateWorkingSetBytes = $privateWorkingSet
                PrivateBytes = [int64]$process.PrivateMemorySize64
                WorkingSetBytes = [int64]$process.WorkingSet64
            }
        }
    }

    return @($snapshots)
}

# Capture reproducibility facts without reading user files or credentials.
function Write-EnvironmentSnapshot {
    param([Parameter(Mandatory)][string]$Path)

    $os = Get-CimInstance Win32_OperatingSystem
    $computer = Get-CimInstance Win32_ComputerSystem
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $gpu = @(Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name)

    # Tool lookup is tolerant because blocked candidates are valid benchmark outcomes.
    $toolVersions = [ordered]@{}
    foreach ($tool in @(
        @{ Name = 'dotnet'; Arguments = @('--version') },
        @{ Name = 'node'; Arguments = @('--version') },
        @{ Name = 'npm'; Arguments = @('--version') },
        @{ Name = 'rustc'; Arguments = @('--version') },
        @{ Name = 'cargo'; Arguments = @('--version') }
    )) {
        $command = Get-Command $tool.Name -ErrorAction SilentlyContinue
        $toolVersions[$tool.Name] = if ($command) {
            (& $command @($tool.Arguments) 2>&1 | Out-String).Trim()
        } else {
            $null
        }
    }

    $snapshot = [ordered]@{
        capturedAt = (Get-Date).ToString('o')
        windows = [ordered]@{
            caption = $os.Caption
            version = $os.Version
            build = $os.BuildNumber
        }
        hardware = [ordered]@{
            manufacturer = $computer.Manufacturer
            model = $computer.Model
            cpu = $cpu.Name.Trim()
            cores = $cpu.NumberOfCores
            logicalProcessors = $cpu.NumberOfLogicalProcessors
            memoryBytes = [int64]$computer.TotalPhysicalMemory
            gpu = $gpu
        }
        shell = $PSVersionTable.PSVersion.ToString()
        powerScheme = (& powercfg /getactivescheme 2>&1 | Out-String).Trim()
        tools = $toolVersions
    }

    $snapshot | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Path -Encoding UTF8
}

# Send one JSON command to a candidate pipe and parse its response.
function Invoke-CandidateCommand {
    param(
        [Parameter(Mandatory)][string]$Candidate,
        [Parameter(Mandatory)][string]$Command,
        [string]$Value,
        [int]$TimeoutMilliseconds = 5000
    )

    $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
        '.',
        "quicknote-stack-$Candidate",
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::None)
    try {
        $pipe.Connect($TimeoutMilliseconds)
        $writer = [System.IO.StreamWriter]::new($pipe, [System.Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [System.IO.StreamReader]::new($pipe, [System.Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $request = [ordered]@{ id = [Guid]::NewGuid().ToString('N'); command = $Command }
            if ($PSBoundParameters.ContainsKey('Value')) {
                $request.value = $Value
            }
            $writer.WriteLine(($request | ConvertTo-Json -Compress))
            $writer.Flush()
            $line = $reader.ReadLine()
            if ([string]::IsNullOrWhiteSpace($line)) {
                throw "Candidate $Candidate returned an empty pipe response."
            }
            return ($line | ConvertFrom-Json)
        } finally {
            try { $reader.Dispose() } catch { }
            try { $writer.Dispose() } catch { }
        }
    } finally {
        try { $pipe.Dispose() } catch { }
    }
}

# Wait for a candidate pipe and preserve the last connection error for diagnosis.
function Wait-CandidateReady {
    param(
        [Parameter(Mandatory)][string]$Candidate,
        [int]$TimeoutMilliseconds = 5000
    )

    $timer = [Diagnostics.Stopwatch]::StartNew()
    $lastError = $null
    while ($timer.ElapsedMilliseconds -lt $TimeoutMilliseconds) {
        try {
            $status = Invoke-CandidateCommand -Candidate $Candidate -Command 'status' -TimeoutMilliseconds 250
            if ([int64]$status.showSequence -ge 1 -and [int64]$status.sentinelAcceptedTicks -gt 0) {
                return $status
            }
        } catch {
            $lastError = $_
        }
        Start-Sleep -Milliseconds 20
    }
    throw "Candidate $Candidate was not ready after $TimeoutMilliseconds ms. Last error: $lastError"
}

Export-ModuleMember -Function Get-BenchmarkRoot, Get-NearestRankPercentile, Get-ProcessTreeSnapshot, Write-EnvironmentSnapshot, Invoke-CandidateCommand, Wait-CandidateReady
