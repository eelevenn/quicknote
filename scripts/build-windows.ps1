$ErrorActionPreference = 'Stop'

# 始终从仓库根目录执行，避免调用方当前目录影响产物位置。
$repositoryRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repositoryRoot

try {
    cargo windows-release
    if ($LASTEXITCODE -ne 0) { throw "Windows 构建失败，退出码：$LASTEXITCODE" }
}
finally {
    Pop-Location
}
