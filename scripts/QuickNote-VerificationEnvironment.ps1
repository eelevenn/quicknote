# 两种本机验收共用用户集成保护；环境变量只能隔离文件，不能隔离 Windows 通知。
function Assert-QuickNoteVerificationSafe {
    if (Get-Process -Name QuickNote,quicknote-windows -ErrorAction SilentlyContinue) {
        throw '请先退出正在使用的 QuickNote，再运行隔离验收。'
    }

    # Windows PowerShell 的 WinRT 投影只读查询真实 AUMID；无法查询时拒绝冒险启动空库。
    $probe = @'
$ErrorActionPreference = 'Stop'
try {
    $manager = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    $notifier = $manager::CreateToastNotifier('eelevenn.QuickNote')
    [int]$notifier.GetScheduledToastNotifications().Count
} catch {
    Write-Error $_
    exit 1
}
'@
    $encodedProbe = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
    $probeOutput = & "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -EncodedCommand $encodedProbe 2>&1
    $count = 0
    if ($LASTEXITCODE -ne 0 -or -not [int]::TryParse(($probeOutput -join "`n").Trim(), [ref]$count) -or $count -lt 0) {
        throw "无法确认 QuickNote 系统计划提醒为空，拒绝运行隔离验收：$($probeOutput -join ' ')"
    }
    if ($count -gt 0) {
        throw "当前用户仍有 $count 条 QuickNote 系统计划提醒。请在专用测试账户运行验收，避免空库协调取消真实提醒。"
    }
}

# 只允许这三个产品专属键进入备份和恢复流程。
function Get-QuickNoteVerificationRegistryPaths {
    @('HKCU\Software\Classes\quicknote', 'HKCU\Software\Classes\AppUserModelId\eelevenn.QuickNote', 'HKCU\Software\Classes\CLSID\{7E5ACBFA-9501-4F8A-9C85-60C1AE5D17C4}')
}

function Save-QuickNoteVerificationRegistry {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $copies = @()
    foreach ($path in Get-QuickNoteVerificationRegistryPaths) {
        $providerPath = 'Registry::HKEY_CURRENT_USER\' + $path.Substring(5)
        $existed = Test-Path -LiteralPath $providerPath -ErrorAction Stop
        $copy = Join-Path $Directory ("registry-$($copies.Count).reg")
        if ($existed) {
            & reg.exe export $path $copy /y 2>$null | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "无法备份已有注册，验收尚未启动：$path" }
        }
        $copies += [pscustomobject]@{ Path = $path; Copy = $copy; Existed = $existed }
    }
    $copies
}

function Restore-QuickNoteVerificationRegistry {
    param([object[]]$Copies = @())

    $allowed = @(Get-QuickNoteVerificationRegistryPaths)
    $failures = @()
    foreach ($entry in $Copies) {
        if ($allowed -notcontains $entry.Path) { throw '拒绝恢复非 QuickNote 注册表路径。' }
        $providerPath = 'Registry::HKEY_CURRENT_USER\' + $entry.Path.Substring(5)
        try {
            if (Test-Path -LiteralPath $providerPath -ErrorAction Stop) {
                & reg.exe delete $entry.Path /f 2>$null | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "无法移除验收注册：$($entry.Path)" }
            }
            if ($entry.Existed) {
                & reg.exe import $entry.Copy 2>$null | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "无法恢复原注册：$($entry.Path)" }
            }
        }
        catch { $failures += $_.Exception.Message }
    }
    if ($failures.Count -gt 0) { throw ($failures -join '；') }
}

# finally 中先恢复集成，失败证据复制到报告目录后才删除本次创建的临时目录。
function Complete-QuickNoteVerification {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$OutputDirectory,
        [object[]]$RegistryCopies = @(),
        [bool]$Succeeded = $false
    )

    $cleanupError = $null
    $evidenceDirectory = $null
    $retained = $true
    try { Restore-QuickNoteVerificationRegistry -Copies $RegistryCopies }
    catch { $cleanupError = $_.Exception.Message }
    try {
        # 仅删除系统临时目录的直属随机子目录；拒绝重解析点和计算越界。
        $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
        $resolvedDirectory = [IO.Path]::GetFullPath($Directory).TrimEnd('\', '/')
        $parent = [IO.Path]::GetDirectoryName($resolvedDirectory)
        $leaf = [IO.Path]::GetFileName($resolvedDirectory)
        if (-not $parent.Equals($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
            $leaf -notmatch '^quicknote-(mvp|shell)-[0-9a-f]{32}$' -or
            ((Get-Item -LiteralPath $resolvedDirectory).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "拒绝清理不属于本次验收的目录：$resolvedDirectory"
        }
        if (-not $Succeeded -or $cleanupError) {
            $evidenceDirectory = Join-Path $OutputDirectory ("failure-data-$leaf")
            Copy-Item -LiteralPath $resolvedDirectory -Destination $evidenceDirectory -Recurse -Force
        }
        Remove-Item -LiteralPath $resolvedDirectory -Recurse -Force
        $retained = $false
    }
    catch {
        $cleanupError = (@($cleanupError, $_.Exception.Message) | Where-Object { $_ }) -join '；'
    }
    [pscustomobject]@{ Error = $cleanupError; EvidenceDirectory = $evidenceDirectory; Retained = $retained }
}
