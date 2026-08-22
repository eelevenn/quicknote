[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Path,

    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\release-candidate'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

function Find-Dumpbin {
    # 优先复用当前开发者终端中的工具，再定位已安装的 VS x64 工具链。
    $command = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw '未找到 vswhere.exe，无法检查 PE 导入表。'
    }
    $visualStudio = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $visualStudio) {
        throw '未找到带 x64 C++ 工具的 Visual Studio。'
    }
    $candidate = Get-ChildItem -LiteralPath (Join-Path $visualStudio 'VC\Tools\MSVC') -Directory |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'bin\Hostx64\x64\dumpbin.exe' } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
    if (-not $candidate) {
        throw '未找到 x64 dumpbin.exe。'
    }
    return $candidate
}

function Get-ImportedDll {
    param(
        [Parameter(Mandatory = $true)][string]$Dumpbin,
        [Parameter(Mandatory = $true)][string]$Binary
    )

    $output = & $Dumpbin /nologo /dependents $Binary 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "dumpbin 无法检查 $Binary，退出码：$LASTEXITCODE"
    }
    # /dependents 的依赖行只包含缩进后的 DLL 文件名。
    @($output | ForEach-Object {
        if ($_ -match '^\s+([A-Za-z0-9._-]+\.dll)\s*$') {
            $Matches[1].ToUpperInvariant()
        }
    } | Where-Object { $_ } | Sort-Object -Unique)
}

$dumpbin = Find-Dumpbin
$forbiddenPattern = '^(VCRUNTIME|MSVCP|CONCRT)[0-9].*\.DLL$'
$items = @()
$failures = @()
foreach ($binaryPath in $Path) {
    $resolved = (Resolve-Path -LiteralPath $binaryPath).ProviderPath
    $dependencies = @(Get-ImportedDll -Dumpbin $dumpbin -Binary $resolved)
    $forbidden = @($dependencies | Where-Object { $_ -match $forbiddenPattern })
    if ($forbidden.Count -gt 0) {
        $failures += "${resolved}: $($forbidden -join ', ')"
    }
    $items += [ordered]@{
        path = $resolved
        sha256 = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
        imported_dlls = $dependencies
        forbidden_vc_runtime_dlls = $forbidden
    }
}

$result = [ordered]@{
    schema_version = 1
    measured_at = [DateTime]::UtcNow.ToString('o')
    dumpbin = $dumpbin
    policy = 'No dynamic VCRUNTIME, MSVCP, or CONCRT dependency; UCRT is part of supported Windows.'
    binaries = $items
}
$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$outputPath = Join-Path $OutputDirectory "runtime-dependencies-$timestamp.json"
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
$result | ConvertTo-Json -Depth 8
Write-Output "Result: $outputPath"

if ($failures.Count -gt 0) {
    throw "发现不允许的动态 VC Runtime 依赖：$($failures -join '; ')"
}
