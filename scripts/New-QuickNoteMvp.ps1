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

# 每次从全新暂存目录构建，旧便携目录中的遗留文件绝不进入新交付物。
$portableName = "QuickNote-$($build.Version)-x64"
$portable = Join-Path $OutputDirectory $portableName
$zip = Join-Path $OutputDirectory "QuickNote-$($build.Version)-x64-portable.zip"
$stage = Join-Path $OutputDirectory ('.quicknote-mvp-stage-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    $stagedPortable = Join-Path $stage $portableName
    $stagedZip = Join-Path $stage 'portable.zip'
    New-Item -ItemType Directory -Path $stagedPortable | Out-Null
    Copy-Item -LiteralPath $build.Executable -Destination (Join-Path $stagedPortable 'QuickNote.exe')
    $documents = @('LICENSE', 'PRIVACY.md', 'THIRD_PARTY_NOTICES.md', 'CODE_SIGNING_POLICY.md')
    foreach ($document in $documents) {
        Copy-Item -LiteralPath (Join-Path $repositoryRoot $document) -Destination $stagedPortable
    }
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'docs\mvp-v0.1.md') -Destination (Join-Path $stagedPortable '使用说明.md')
    Compress-Archive -LiteralPath $stagedPortable -DestinationPath $stagedZip

    # 逐项验证 ZIP 白名单，防止语音 sidecar、测试文件或旧版本载荷混入。
    $expected = @(@('QuickNote.exe', '使用说明.md') + $documents | ForEach-Object { "$portableName/$_" })
    $archive = [IO.Compression.ZipFile]::OpenRead($stagedZip)
    try {
        $actual = @($archive.Entries | Where-Object { -not $_.FullName.EndsWith('/') } | ForEach-Object { $_.FullName.Replace('\', '/') })
        if ($actual.Count -ne $expected.Count -or @(Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive).Count -gt 0) {
            throw '便携 ZIP 内容与预期文件白名单不一致。'
        }
    }
    finally { $archive.Dispose() }

    # 发布前验证旧产物是输出目录的直属产品目录，且不是指向用户数据的重解析点。
    if (Test-Path -LiteralPath $portable) {
        $existing = Get-Item -LiteralPath $portable
        if (-not $existing.PSIsContainer -or ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            -not [IO.Path]::GetDirectoryName($existing.FullName).Equals($OutputDirectory.TrimEnd('\', '/'), [StringComparison]::OrdinalIgnoreCase) -or
            $existing.Name -ne $portableName) {
            throw "拒绝替换未确认的便携目录：$portable"
        }
        Remove-Item -LiteralPath $existing.FullName -Recurse -Force
    }
    Move-Item -LiteralPath $stagedPortable -Destination $portable
    Move-Item -LiteralPath $stagedZip -Destination $zip -Force
}
finally {
    # stage 名称由本次随机生成；清理前再次限制到输出目录直属子目录。
    $resolvedStage = [IO.Path]::GetFullPath($stage)
    if ([IO.Path]::GetDirectoryName($resolvedStage).Equals($OutputDirectory.TrimEnd('\', '/'), [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($resolvedStage) -match '^\.quicknote-mvp-stage-[0-9a-f]{32}$' -and
        -not ((Get-Item -LiteralPath $resolvedStage).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
}

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
