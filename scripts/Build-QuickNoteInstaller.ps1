[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [string]$ExecutablePath,

    [string]$OutputDirectory,

    [switch]$SkipApplicationBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 所有输入都转换为绝对本地路径，避免 WiX 从 WSL UNC 当前目录解析相对路径。
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installerSource = Join-Path $repositoryRoot 'installer\QuickNote.wxs'
if (-not $Version) {
    # 默认复用 workspace 版本，避免 CI 与应用 About/安装器版本漂移。
    $cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -Raw
    if ($cargoManifest -notmatch '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"\s*$') {
        throw '无法从 workspace Cargo.toml 读取三段版本号。'
    }
    $Version = $Matches.version
}
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\release-candidate'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

if (-not $SkipApplicationBuild) {
    & (Join-Path $PSScriptRoot 'build-windows.ps1')
}
if (-not $ExecutablePath) {
    $ExecutablePath = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-windows.exe'
}
$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).ProviderPath

function New-DeterministicProductCode {
    param([Parameter(Mandatory = $true)][string]$ProductVersion)

    # 同一版本必须得到相同 ProductCode，才能让重复安装进入 MSI 维修模式。
    $payload = [Text.Encoding]::UTF8.GetBytes("eelevenn.QuickNote/msi/$ProductVersion")
    $digest = [Security.Cryptography.SHA256]::HashData($payload)
    $guidBytes = [byte[]]$digest[0..15]
    # 设置 RFC 4122 variant 和版本位；这里只要求确定性，不把它用作安全标识。
    $guidBytes[7] = ($guidBytes[7] -band 0x0F) -bor 0x50
    $guidBytes[8] = ($guidBytes[8] -band 0x3F) -bor 0x80
    ([guid]::new($guidBytes)).ToString('B').ToUpperInvariant()
}

# Windows 工具会阻止直接来自 WSL UNC 的清单和载荷，因此只在本地临时目录编译。
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$stage = Join-Path $temporaryRoot ('quicknote-installer-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $stage '.config') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repositoryRoot '.config\dotnet-tools.json') -Destination (Join-Path $stage '.config\dotnet-tools.json')
Copy-Item -LiteralPath $installerSource -Destination (Join-Path $stage 'QuickNote.wxs')
Copy-Item -LiteralPath $resolvedExecutable -Destination (Join-Path $stage 'QuickNote.exe')
Get-ChildItem -LiteralPath $stage -Recurse -File | Unblock-File

# 使用仓库固定的工具清单，避免开发机全局 WiX 版本改变产物。
$stagedOutput = Join-Path $stage "QuickNote-$Version-x64.msi"
Push-Location -LiteralPath $stage
try {
    dotnet tool restore
    if ($LASTEXITCODE -ne 0) {
        throw "WiX 工具恢复失败，退出码：$LASTEXITCODE"
    }

    $productCode = New-DeterministicProductCode -ProductVersion $Version
    $outputPath = Join-Path $OutputDirectory "QuickNote-$Version-x64.msi"
    dotnet tool run wix build `
        (Join-Path $stage 'QuickNote.wxs') `
        -arch x64 `
        -d "ProductVersion=$Version" `
        -d "ProductCode=$productCode" `
        -d "QuickNoteExecutable=$(Join-Path $stage 'QuickNote.exe')" `
        -out $stagedOutput
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $stagedOutput)) {
        throw "WiX MSI 构建失败，退出码：$LASTEXITCODE"
    }
    Copy-Item -LiteralPath $stagedOutput -Destination $outputPath -Force
}
finally {
    Pop-Location
    # 只删除本脚本在系统临时目录中以随机 GUID 创建的目录。
    $resolvedStage = [IO.Path]::GetFullPath($stage)
    if ($resolvedStage.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
}

$installer = Get-Item -LiteralPath $outputPath
if ($installer.Length -gt 18MB) {
    throw "安装器体积 $($installer.Length) bytes 超过 18 MiB 硬门槛。"
}

[ordered]@{
    installer = $installer.FullName
    installer_bytes = $installer.Length
    executable = $resolvedExecutable
    executable_bytes = (Get-Item -LiteralPath $resolvedExecutable).Length
    version = $Version
    product_code = $productCode
} | ConvertTo-Json -Depth 4
