# 从 Cargo 读取实际构建目录，兼容 CARGO_TARGET_DIR 和本机 .cargo/config.toml。
function Get-QuickNoteBuild {
    param([Parameter(Mandatory = $true)][string]$RepositoryRoot)

    $metadataText = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $RepositoryRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw '无法读取 Cargo workspace 元数据。' }
    $metadata = $metadataText | ConvertFrom-Json
    $application = @($metadata.packages | Where-Object name -eq 'quicknote-windows')[0]
    [pscustomobject]@{
        Version = $application.version
        Executable = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc\release\quicknote-windows.exe'
    }
}
