[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [switch]$SkipApplicationBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'Get-QuickNoteBuild.ps1')
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'artifacts\mvp' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

# 开发交付无需签名凭据；仍检查真实 Release 载荷、运行时依赖和 MSI 表。
if (-not $SkipApplicationBuild) { & (Join-Path $PSScriptRoot 'build-windows.ps1') }
$build = Get-QuickNoteBuild -RepositoryRoot $repositoryRoot
if (-not (Test-Path -LiteralPath $build.Executable)) { throw '找不到 Release 应用，请先构建。' }
& (Join-Path $PSScriptRoot 'Test-WindowsRuntimeDependencies.ps1') -Path $build.Executable -OutputDirectory $OutputDirectory
& (Join-Path $PSScriptRoot 'Build-QuickNoteInstaller.ps1') -Version $build.Version -ExecutablePath $build.Executable -OutputDirectory $OutputDirectory -SkipApplicationBuild
$installer = Join-Path $OutputDirectory "QuickNote-$($build.Version)-x64.msi"
& (Join-Path $PSScriptRoot 'Test-QuickNoteInstaller.ps1') -InstallerPath $installer -OutputDirectory $OutputDirectory -AllowUnsigned

# 独立目录可直接运行；SQLite 数据仍保存在当前用户的 LocalAppData。
$portable = Join-Path $OutputDirectory "QuickNote-$($build.Version)-x64"
New-Item -ItemType Directory -Path $portable -Force | Out-Null
Copy-Item -LiteralPath $build.Executable -Destination (Join-Path $portable 'QuickNote.exe') -Force
foreach ($document in @('LICENSE', 'PRIVACY.md', 'THIRD_PARTY_NOTICES.md', 'CODE_SIGNING_POLICY.md')) {
    Copy-Item -LiteralPath (Join-Path $repositoryRoot $document) -Destination $portable -Force
}
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs\mvp-v0.1.md') -Destination (Join-Path $portable '使用说明.md') -Force
$zip = Join-Path $OutputDirectory "QuickNote-$($build.Version)-x64-portable.zip"
Compress-Archive -LiteralPath $portable -DestinationPath $zip -Force

# 清单显式标记未签名，附精确体积和 SHA-256，便于交付后确认文件一致。
$files = @($installer, $zip, (Join-Path $portable 'QuickNote.exe')) | ForEach-Object {
    $item = Get-Item -LiteralPath $_
    # 可执行文件位于便携目录，记录相对路径后清单才能从输出目录直接校验。
    $relativePath = [IO.Path]::GetRelativePath($OutputDirectory, $item.FullName).Replace('\', '/')
    [ordered]@{ file = $relativePath; bytes = $item.Length; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$manifest = [ordered]@{
    schema_version = 1
    version = $build.Version
    channel = 'unsigned-mvp'
    signed = $false
    created_at = [DateTime]::UtcNow.ToString('o')
    source_worktree = $repositoryRoot
    files = $files
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'manifest.json') -Encoding utf8
$files | ForEach-Object { "$($_.sha256)  $($_.file)" } | Set-Content -LiteralPath (Join-Path $OutputDirectory 'SHA256SUMS.txt') -Encoding ascii
Write-Output "MVP 安装包：$installer"
Write-Output "MVP 便携包：$zip"
