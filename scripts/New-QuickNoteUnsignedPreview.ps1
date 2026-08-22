[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\unsigned-signpath-preview'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

# SignPath 必须能把二进制追溯到公开提交；脏工作树产物不得进入预览 Release。
# Windows 无法表达 WSL Bash 文件的可执行位，因此这里只忽略 file mode 与换行转换。
$gitCompatibility = @('-c', 'core.filemode=false', '-c', 'core.autocrlf=false')
$status = & git @gitCompatibility -C $repositoryRoot status --porcelain
if ($LASTEXITCODE -ne 0) {
    throw '无法读取 Git 工作树状态。'
}
if ($status) {
    throw '未签名预览只能从完全干净的 Git 工作树构建。'
}
$commit = (& git @gitCompatibility -C $repositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') {
    throw '无法确定预览产物对应的 Git 提交。'
}

# 应用、MSI 与清单必须复用同一个 workspace 版本。
$cargoManifest = Get-Content -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -Raw
if ($cargoManifest -notmatch '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"\s*$') {
    throw '无法从 workspace Cargo.toml 读取三段版本号。'
}
if ($Version -ne $Matches.version) {
    throw "预览版本 $Version 与应用版本 $($Matches.version) 不一致。"
}

& (Join-Path $PSScriptRoot 'build-windows.ps1')
$executable = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-windows.exe'
$sidecar = Join-Path $repositoryRoot 'target\x86_64-pc-windows-msvc\release\quicknote-sensevoice-sidecar.exe'
& (Join-Path $PSScriptRoot 'Test-WindowsRuntimeDependencies.ps1') `
    -Path @($executable, $sidecar) `
    -OutputDirectory $OutputDirectory

& (Join-Path $PSScriptRoot 'Build-QuickNoteInstaller.ps1') `
    -Version $Version `
    -ExecutablePath $executable `
    -OutputDirectory $OutputDirectory `
    -SkipApplicationBuild
$installer = Join-Path $OutputDirectory "QuickNote-$Version-x64.msi"
& (Join-Path $PSScriptRoot 'Test-QuickNoteInstaller.ps1') `
    -InstallerPath $installer `
    -OutputDirectory $OutputDirectory `
    -AllowUnsigned

# 这个特殊预览必须保持未签名，避免把测试身份误报为正式发布者。
$installerSignature = Get-AuthenticodeSignature -LiteralPath $installer
$applicationSignature = Get-AuthenticodeSignature -LiteralPath $executable
if ($installerSignature.Status -ne 'NotSigned' -or $applicationSignature.Status -ne 'NotSigned') {
    throw 'SignPath 申请预览必须是明确未签名的 MSI 与应用。'
}

$installerItem = Get-Item -LiteralPath $installer
$manifest = [ordered]@{
    schema_version = 1
    purpose = 'SignPath Foundation eligibility preview; not a production release'
    warning = 'UNSIGNED: Windows SmartScreen and Authenticode trust are expected to fail'
    created_at = [DateTime]::UtcNow.ToString('o')
    source_repository = 'https://github.com/eelevenn/quicknote'
    source_commit = $commit
    version = $Version
    installer = [ordered]@{
        file = $installerItem.Name
        bytes = $installerItem.Length
        sha256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        signature_status = $installerSignature.Status.ToString()
    }
    application_signature_status = $applicationSignature.Status.ToString()
    transcription_sidecar_distribution = 'separate optional package; excluded from this preview MSI'
}
$manifestPath = Join-Path $OutputDirectory "QuickNote-$Version-x64.unsigned-preview.json"
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding utf8

# 独立校验文件便于审核者在下载后验证 MSI 未被替换。
$hashPath = Join-Path $OutputDirectory "QuickNote-$Version-x64.SHA256SUMS.txt"
"$($manifest.installer.sha256)  $($installerItem.Name)" |
    Set-Content -LiteralPath $hashPath -Encoding ascii

$manifest | ConvertTo-Json -Depth 6
Write-Output "Unsigned preview manifest: $manifestPath"
Write-Output "SHA-256 list: $hashPath"
