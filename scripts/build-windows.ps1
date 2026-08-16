$ErrorActionPreference = 'Stop'

# 始终从仓库根目录执行，避免调用方当前目录影响产物位置。
$repositoryRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repositoryRoot

try {
    cargo windows-release
    & (Join-Path $PSScriptRoot 'Build-TranscriptionSidecar.ps1')
}
finally {
    Pop-Location
}
