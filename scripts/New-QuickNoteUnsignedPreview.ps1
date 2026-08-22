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
# GitHub Windows runner 会按平台转换换行，因此只忽略行尾差异与 WSL 文件模式差异。
$gitCompatibility = @('-c', 'core.filemode=false', '-c', 'core.autocrlf=false')
$trackedChanges = & git @gitCompatibility -C $repositoryRoot diff --name-only --ignore-space-at-eol
$stagedChanges = & git @gitCompatibility -C $repositoryRoot diff --cached --name-only --ignore-space-at-eol
$untrackedChanges = & git @gitCompatibility -C $repositoryRoot ls-files --others --exclude-standard
if ($LASTEXITCODE -ne 0) {
    throw '无法读取 Git 工作树状态。'
}
$dirtyEntries = @($trackedChanges) + @($stagedChanges) + @($untrackedChanges) |
    Where-Object { $_ } |
    Sort-Object -Unique
if ($dirtyEntries.Count -gt 0) {
    throw "未签名预览只能从干净的 Git 工作树构建：$($dirtyEntries -join ', ')"
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

# Authenticode 对 UNC 上的 MSI 会返回 UnknownError，因此在本地临时目录验证副本。
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$signatureStage = Join-Path $temporaryRoot ('quicknote-preview-signature-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $signatureStage | Out-Null
try {
    $localInstaller = Join-Path $signatureStage (Split-Path -Leaf $installer)
    $localExecutable = Join-Path $signatureStage (Split-Path -Leaf $executable)
    Copy-Item -LiteralPath $installer -Destination $localInstaller
    Copy-Item -LiteralPath $executable -Destination $localExecutable
    Unblock-File -LiteralPath $localInstaller, $localExecutable
    $installerSignatureStatus = (Get-AuthenticodeSignature -LiteralPath $localInstaller).Status.ToString()
    $applicationSignatureStatus = (Get-AuthenticodeSignature -LiteralPath $localExecutable).Status.ToString()
    if ($installerSignatureStatus -ne 'NotSigned' -or $applicationSignatureStatus -ne 'NotSigned') {
        throw 'SignPath 申请预览必须是明确未签名的 MSI 与应用。'
    }
}
finally {
    # 只删除本脚本在系统临时目录下创建的随机签名验证目录。
    $resolvedSignatureStage = [IO.Path]::GetFullPath($signatureStage)
    if ($resolvedSignatureStage.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedSignatureStage -Recurse -Force
    }
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
        signature_status = $installerSignatureStatus
    }
    application_signature_status = $applicationSignatureStatus
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
