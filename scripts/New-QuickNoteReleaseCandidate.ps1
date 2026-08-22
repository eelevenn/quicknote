[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$CertificateThumbprint,

    [ValidatePattern('^https://')]
    [string]$TimestampUrl = 'https://timestamp.digicert.com',

    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\release-candidate'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

# MSI 与 About 必须来自同一版本源，禁止只改安装器版本制造不可追溯产物。
$cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -Raw
if ($cargoManifest -notmatch '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"\s*$') {
    throw '无法从 workspace Cargo.toml 读取三段版本号。'
}
$applicationVersion = $Matches.version
if ($Version -ne $applicationVersion) {
    throw "发布版本 $Version 与应用版本 $applicationVersion 不一致；请先更新 workspace 版本。"
}

# 发布候选严格按“构建载荷 -> 签全部 PE -> 构建 MSI -> 签 MSI -> 验证”排序。
& (Join-Path $PSScriptRoot 'build-windows.ps1')
$executable = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-windows.exe'
$sidecar = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-sensevoice-sidecar.exe'
& (Join-Path $PSScriptRoot 'Sign-QuickNoteArtifacts.ps1') `
    -Path @($executable, $sidecar) `
    -CertificateThumbprint $CertificateThumbprint `
    -TimestampUrl $TimestampUrl

# 在创建 MSI 前阻止重新引入需要管理员安装的 VC Runtime 依赖。
& (Join-Path $PSScriptRoot 'Test-WindowsRuntimeDependencies.ps1') `
    -Path @($executable, $sidecar) `
    -OutputDirectory $OutputDirectory

& (Join-Path $PSScriptRoot 'Build-QuickNoteInstaller.ps1') `
    -Version $Version `
    -ExecutablePath $executable `
    -OutputDirectory $OutputDirectory `
    -SkipApplicationBuild
$installer = Join-Path $OutputDirectory "QuickNote-$Version-x64.msi"
& (Join-Path $PSScriptRoot 'Sign-QuickNoteArtifacts.ps1') `
    -Path $installer `
    -CertificateThumbprint $CertificateThumbprint `
    -TimestampUrl $TimestampUrl
& (Join-Path $PSScriptRoot 'Test-QuickNoteInstaller.ps1') `
    -InstallerPath $installer `
    -OutputDirectory $OutputDirectory

$manifest = [ordered]@{
    schema_version = 1
    created_at = [DateTime]::UtcNow.ToString('o')
    version = $Version
    supported_os = 'Windows 11 25H2 or newer, x64'
    installer = [ordered]@{
        path = $installer
        bytes = (Get-Item -LiteralPath $installer).Length
        sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    application = [ordered]@{
        path = $executable
        bytes = (Get-Item -LiteralPath $executable).Length
        sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    transcription_sidecar = [ordered]@{
        path = $sidecar
        bytes = (Get-Item -LiteralPath $sidecar).Length
        sha256 = (Get-FileHash -LiteralPath $sidecar -Algorithm SHA256).Hash.ToLowerInvariant()
        distribution = 'separate on-demand transcription package; excluded from MSI'
    }
    signing_certificate_thumbprint = $CertificateThumbprint.ToUpperInvariant()
    timestamp_url = $TimestampUrl
}
$manifestPath = Join-Path $OutputDirectory "QuickNote-$Version-x64.release.json"
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding utf8
$manifest | ConvertTo-Json -Depth 6
Write-Output "Release manifest: $manifestPath"
