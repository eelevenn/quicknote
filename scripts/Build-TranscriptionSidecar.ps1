param(
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

# 构建只缓存固定 sherpa-onnx SDK，最终应用目录不会复制模型或运行时。
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repositoryRoot 'crates\quicknote-app\assets\transcription-package.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$runtime = $manifest.assets | Where-Object name -EQ 'sherpa-onnx-runtime' | Select-Object -First 1
if (-not $runtime) {
    throw '固定包清单缺少 sherpa-onnx runtime。'
}

$cacheRoot = Join-Path $repositoryRoot '.scratch\transcription-sidecar-build'
$archive = Join-Path $cacheRoot $runtime.fileName
$sdkRoot = Join-Path $cacheRoot 'sdk'
New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null

if (-not (Test-Path -LiteralPath $archive)) {
    Invoke-WebRequest -Uri $runtime.url -OutFile $archive -UseBasicParsing
}
$archiveItem = Get-Item -LiteralPath $archive
$archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($archiveItem.Length -ne [Int64]$runtime.compressedBytes -or $archiveHash -ne $runtime.sha256) {
    throw '固定 sherpa-onnx SDK 大小或 SHA-256 不匹配。'
}

if (-not (Test-Path -LiteralPath $sdkRoot)) {
    New-Item -ItemType Directory -Path $sdkRoot | Out-Null
    & tar.exe -xjf $archive -C $sdkRoot
    if ($LASTEXITCODE -ne 0) {
        throw "sherpa-onnx SDK 解压失败，退出码：$LASTEXITCODE"
    }
}
$runtimeRoot = Get-ChildItem -LiteralPath $sdkRoot -Directory |
    Where-Object Name -Like 'sherpa-onnx-*-win-x64-shared-MT-Release-no-tts' |
    Select-Object -First 1
if (-not $runtimeRoot) {
    throw '固定 SDK 中未找到 sherpa-onnx Windows x64 运行时。'
}

$vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    throw '未找到 Visual Studio Installer，无法构建原生 sidecar。'
}
$visualStudio = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $visualStudio) {
    throw '未找到 MSVC x64 工具链。'
}

if (-not $OutputPath) {
    $OutputPath = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-sensevoice-sidecar.exe'
}
$outputDirectory = Split-Path -Parent $OutputPath
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
$source = Join-Path $repositoryRoot 'apps\quicknote-windows\native\sensevoice_sidecar.cpp'
$include = Join-Path $runtimeRoot.FullName 'include'
$library = Join-Path $runtimeRoot.FullName 'lib\sherpa-onnx-c-api.lib'
$developerShell = Join-Path $visualStudio 'VC\Auxiliary\Build\vcvars64.bat'
$object = Join-Path $outputDirectory 'quicknote-sensevoice-sidecar.obj'
$programDatabase = Join-Path $outputDirectory 'quicknote-sensevoice-sidecar.pdb'

# 显式指定中间产物，避免 cmd 在 UNC 工作目录回退到 Windows 系统目录。
$compile = '"{0}" && cl.exe /nologo /O2 /EHsc /std:c++17 /utf-8 /Fo"{1}" /Fd"{2}" "{3}" /I"{4}" /link "{5}" /OUT:"{6}"' -f $developerShell, $object, $programDatabase, $source, $include, $library, $OutputPath
& cmd.exe /d /s /c $compile
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $OutputPath)) {
    throw "SenseVoice sidecar 构建失败，退出码：$LASTEXITCODE"
}
Write-Host "SenseVoice sidecar: $OutputPath"
