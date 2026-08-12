[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('tauri', 'slint')]
    [string]$Candidate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$benchmarkRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
$rustBin = 'C:\Program Files\Rust stable MSVC 1.97\bin'
if (Test-Path -LiteralPath $rustBin) {
    $env:Path = "$rustBin;$env:Path"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo is missing. Install the stable x64-pc-windows-msvc toolchain first.'
}

# Windows toolchains do not reliably build from a WSL UNC working directory.
$stageRoot = Join-Path $env:TEMP "quicknote-stack-build/$Candidate"
if (Test-Path -LiteralPath $stageRoot) {
    Remove-Item -LiteralPath $stageRoot -Recurse -Force
}
New-Item -ItemType Directory -Path $stageRoot -Force | Out-Null
$candidateRoot = Join-Path $stageRoot $Candidate
$supportRoot = Join-Path $stageRoot 'rust-support'
New-Item -ItemType Directory -Path $candidateRoot, $supportRoot -Force | Out-Null

# Copy only source inputs so stale build caches cannot affect the staged build.
Get-ChildItem -LiteralPath (Join-Path $benchmarkRoot $Candidate) -Force |
    Where-Object Name -NotIn @('bin', 'obj', 'node_modules', 'target') |
    Copy-Item -Destination $candidateRoot -Recurse
Get-ChildItem -LiteralPath (Join-Path $benchmarkRoot 'rust-support') -Force |
    Where-Object Name -NotIn @('target') |
    Copy-Item -Destination $supportRoot -Recurse

if ($Candidate -eq 'tauri') {
    Push-Location $candidateRoot
    try {
        & npm ci
        if ($LASTEXITCODE -ne 0) { throw "npm ci failed with exit code $LASTEXITCODE." }
        & npm run icon
        if ($LASTEXITCODE -ne 0) { throw "Tauri icon generation failed with exit code $LASTEXITCODE." }
        Copy-Item -LiteralPath (Join-Path $candidateRoot 'index.html') -Destination (Join-Path $candidateRoot 'dist/index.html') -Force
        & npm run tauri build -- --no-bundle
        if ($LASTEXITCODE -ne 0) { throw "Tauri build failed with exit code $LASTEXITCODE." }
    } finally {
        Pop-Location
    }
    $sourceExecutable = Join-Path $candidateRoot 'src-tauri/target/release/quicknote-stack-tauri.exe'
    $destinationDirectory = Join-Path $benchmarkRoot 'tauri/src-tauri/target/release'
} else {
    & cargo build --manifest-path (Join-Path $candidateRoot 'Cargo.toml') --release --locked
    if ($LASTEXITCODE -ne 0) { throw "Slint build failed with exit code $LASTEXITCODE." }
    $sourceExecutable = Join-Path $candidateRoot 'target/release/quicknote-stack-slint.exe'
    $destinationDirectory = Join-Path $benchmarkRoot 'slint/target/release'
}

New-Item -ItemType Directory -Path $destinationDirectory -Force | Out-Null
Copy-Item -LiteralPath $sourceExecutable -Destination $destinationDirectory -Force
Write-Host "Built ${Candidate}: $sourceExecutable"
