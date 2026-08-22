[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,

    [string]$PreviousInstallerPath,

    [string]$OutputDirectory,

    [switch]$InstallLifecycle,

    [switch]$AllowUnsigned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient

$resolvedInstaller = (Resolve-Path -LiteralPath $InstallerPath).ProviderPath
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repositoryRoot 'artifacts\release-candidate'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

# Windows Installer COM 和 msiexec 会阻止 WSL UNC 载荷，因此在本地临时目录验证副本。
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$validationStage = Join-Path $temporaryRoot ('quicknote-msi-validation-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $validationStage -Force | Out-Null
$localInstaller = Join-Path $validationStage (Split-Path -Leaf $resolvedInstaller)
Copy-Item -LiteralPath $resolvedInstaller -Destination $localInstaller
Unblock-File -LiteralPath $localInstaller
$localPreviousInstaller = $null
if ($PreviousInstallerPath) {
    $resolvedPreviousInstaller = (Resolve-Path -LiteralPath $PreviousInstallerPath).ProviderPath
    $localPreviousInstaller = Join-Path $validationStage ('previous-' + (Split-Path -Leaf $resolvedPreviousInstaller))
    Copy-Item -LiteralPath $resolvedPreviousInstaller -Destination $localPreviousInstaller
    Unblock-File -LiteralPath $localPreviousInstaller
}

function Open-MsiDatabase {
    param([Parameter(Mandatory = $true)][string]$Path)

    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $installer.GetType().InvokeMember(
        'OpenDatabase',
        'InvokeMethod',
        $null,
        $installer,
        @($Path, 0)
    )
    [ordered]@{ installer = $installer; database = $database }
}

function Read-MsiScalar {
    param(
        [Parameter(Mandatory = $true)]$Database,
        [Parameter(Mandatory = $true)][string]$Query
    )

    $view = $Database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $Database, @($Query))
    $record = $null
    try {
        $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
        $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if (-not $record) {
            return $null
        }
        $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, 1)
    }
    finally {
        if ($record) {
            [Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) | Out-Null
        }
        $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    }
}

function Read-MsiProperty {
    param($Database, [string]$Name)

    # 属性名来自本脚本的固定白名单，不接收任意 SQL 输入。
    Read-MsiScalar -Database $Database -Query "SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'"
}

function Assert-Equal {
    param([string]$Name, $Actual, $Expected)

    if ($Actual -ne $Expected) {
        throw "$Name 不符合预期：实际 '$Actual'，预期 '$Expected'。"
    }
}

function Invoke-MsiExec {
    param([string[]]$Arguments, [string]$LogPath)

    # 调用运算符保留每个路径参数的边界，兼容用户名和日志目录中的空格。
    $allArguments = $Arguments + @('/qn', '/norestart', '/l*v', $LogPath)
    & "$env:SystemRoot\System32\msiexec.exe" @allArguments
    if ($LASTEXITCODE -ne 0) {
        throw "msiexec 失败，退出码：$LASTEXITCODE，日志：$LogPath"
    }
}

function Wait-ForEditableControl {
    param([System.Diagnostics.Process]$Process, [int]$TimeoutMilliseconds = 8000)

    if (-not $Process.WaitForInputIdle($TimeoutMilliseconds)) {
        throw 'QuickNote 未在超时前进入可输入空闲状态。'
    }
    $processCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty,
        $Process.Id
    )
    $editCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Edit
    )
    $condition = [System.Windows.Automation.AndCondition]::new($processCondition, $editCondition)
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        $editor = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            $condition
        )
        if ($editor) {
            return
        }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'QuickNote 未在超时前暴露可编辑 UIA 控件。'
}

$msi = $null
try {
$msi = Open-MsiDatabase -Path $localInstaller
$productName = Read-MsiProperty -Database $msi.database -Name 'ProductName'
$productVersion = Read-MsiProperty -Database $msi.database -Name 'ProductVersion'
$productCode = Read-MsiProperty -Database $msi.database -Name 'ProductCode'
$upgradeCode = Read-MsiProperty -Database $msi.database -Name 'UpgradeCode'
$allUsers = Read-MsiProperty -Database $msi.database -Name 'ALLUSERS'
Assert-Equal -Name 'ProductName' -Actual $productName -Expected 'QuickNote'
Assert-Equal -Name 'UpgradeCode' -Actual $upgradeCode -Expected '{22D55004-7823-4192-A3D2-046F408953B1}'
if ($allUsers) {
    throw "固定每用户 MSI 不得设置 ALLUSERS，实际值：$allUsers"
}

$shortcutAumid = Read-MsiScalar -Database $msi.database -Query "SELECT ``PropVariantValue`` FROM ``MsiShortcutProperty`` WHERE ``PropertyKey`` = 'System.AppUserModel.ID'"
$shortcutActivator = Read-MsiScalar -Database $msi.database -Query "SELECT ``PropVariantValue`` FROM ``MsiShortcutProperty`` WHERE ``PropertyKey`` = 'System.AppUserModel.ToastActivatorCLSID'"
Assert-Equal -Name '快捷方式 AUMID' -Actual $shortcutAumid -Expected 'eelevenn.QuickNote'
Assert-Equal -Name '通知激活 CLSID' -Actual $shortcutActivator -Expected '{7E5ACBFA-9501-4F8A-9C85-60C1AE5D17C4}'

# Major Upgrade 必须在事务开始后、复制新文件前移除旧版，才能回滚旧产品。
$installInitializeSequence = [int](Read-MsiScalar -Database $msi.database -Query "SELECT ``Sequence`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = 'InstallInitialize'")
$removeExistingProductsSequence = [int](Read-MsiScalar -Database $msi.database -Query "SELECT ``Sequence`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = 'RemoveExistingProducts'")
$installFilesSequence = [int](Read-MsiScalar -Database $msi.database -Query "SELECT ``Sequence`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = 'InstallFiles'")
if ($removeExistingProductsSequence -le $installInitializeSequence -or
    $removeExistingProductsSequence -ge $installFilesSequence) {
    throw "RemoveExistingProducts 排程不在事务开始与文件安装之间：$removeExistingProductsSequence"
}
$cleanupCondition = Read-MsiScalar -Database $msi.database -Query "SELECT ``Condition`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = 'RemoveUserIntegration'"
Assert-Equal `
    -Name '最终卸载清理条件' `
    -Actual $cleanupCondition `
    -Expected 'REMOVE~="ALL" AND NOT UPGRADINGPRODUCTCODE'

$installerItem = Get-Item -LiteralPath $resolvedInstaller
if ($installerItem.Length -gt 18MB) {
    throw "安装器体积 $($installerItem.Length) bytes 超过 18 MiB。"
}
$signature = Get-AuthenticodeSignature -LiteralPath $localInstaller
if (-not $AllowUnsigned -and $signature.Status -ne 'Valid') {
    throw "安装器签名状态不是 Valid：$($signature.Status)"
}

$result = [ordered]@{
    measured_at = [DateTime]::UtcNow.ToString('o')
    installer = $resolvedInstaller
    installer_bytes = $installerItem.Length
    installer_sha256 = (Get-FileHash -LiteralPath $resolvedInstaller -Algorithm SHA256).Hash.ToLowerInvariant()
    signature_status = $signature.Status.ToString()
    product_name = $productName
    product_version = $productVersion
    product_code = $productCode
    upgrade_code = $upgradeCode
    remove_existing_products_sequence = $removeExistingProductsSequence
    uninstall_cleanup_condition = $cleanupCondition
    static_checks = 'passed'
    lifecycle_checks = if ($InstallLifecycle) { 'running' } else { 'not_requested' }
}

if ($InstallLifecycle) {
    if ([Environment]::OSVersion.Version.Build -lt 26200) {
        throw '安装生命周期验收需要 Windows 11 25H2（build 26200）或更新版本。'
    }
    $dataDirectory = Join-Path $env:LOCALAPPDATA 'QuickNote'
    if (Test-Path -LiteralPath $dataDirectory) {
        throw "为避免接触现有用户数据，生命周期验收要求干净配置文件：$dataDirectory"
    }

    $timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $installDirectory = Join-Path $env:LOCALAPPDATA 'Programs\QuickNote'
    $installedExecutable = Join-Path $installDirectory 'QuickNote.exe'
    $startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\QuickNote.lnk'
    $testEnvironment = Join-Path ([IO.Path]::GetTempPath()) ('quicknote-release-test-' + [guid]::NewGuid().ToString('N'))
    $testLocalAppData = Join-Path $testEnvironment 'LocalAppData'
    $testAppData = Join-Path $testEnvironment 'AppData'
    New-Item -ItemType Directory -Path $testLocalAppData, $testAppData -Force | Out-Null

    if ($PreviousInstallerPath) {
        Invoke-MsiExec -Arguments @('/i', $localPreviousInstaller) -LogPath (Join-Path $OutputDirectory "$timestamp-upgrade-old-install.log")
        # 模拟用户已启用登录启动；稳定安装路径使覆盖升级无需改写该值。
        $runKey = New-Item -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Force
        $runKey | Set-ItemProperty -Name 'QuickNote' -Value "`"$installedExecutable`" --startup"
    }

    Invoke-MsiExec -Arguments @('/i', $localInstaller) -LogPath (Join-Path $OutputDirectory "$timestamp-install.log")
    if (-not (Test-Path -LiteralPath $installedExecutable)) {
        throw '安装后未找到 QuickNote.exe。'
    }
    if (-not (Test-Path -LiteralPath $startMenuShortcut)) {
        throw '安装后未找到开始菜单快捷方式。'
    }
    Assert-Equal `
        -Name '协议命令' `
        -Actual (Get-ItemPropertyValue -LiteralPath 'HKCU:\Software\Classes\quicknote\shell\open\command') `
        -Expected "`"$installedExecutable`" `"%1`""

    $application = Start-Process `
        -FilePath $installedExecutable `
        -ArgumentList '--quick-capture' `
        -Environment @{ LOCALAPPDATA = $testLocalAppData; APPDATA = $testAppData } `
        -PassThru
    try {
        Wait-ForEditableControl -Process $application
    }
    finally {
        if (-not $application.HasExited) {
            $application.Kill()
            $application.WaitForExit()
        }
        $application.Dispose()
    }

    # 同版本重复安装必须是确定性的维修操作。
    $installedHash = (Get-FileHash -LiteralPath $installedExecutable -Algorithm SHA256).Hash
    Invoke-MsiExec -Arguments @('/fa', $productCode) -LogPath (Join-Path $OutputDirectory "$timestamp-repair.log")
    Assert-Equal `
        -Name '维修后应用哈希' `
        -Actual (Get-FileHash -LiteralPath $installedExecutable -Algorithm SHA256).Hash `
        -Expected $installedHash

    if ($PreviousInstallerPath) {
        $startupCommand = Get-ItemPropertyValue `
            -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' `
            -Name 'QuickNote'
        Assert-Equal -Name '升级后自启动命令' -Actual $startupCommand -Expected "`"$installedExecutable`" --startup"
    }

    $sentinel = Join-Path $dataDirectory '.release-validation-sentinel'
    New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
    Set-Content -LiteralPath $sentinel -Value 'uninstall must preserve user data' -Encoding utf8
    Invoke-MsiExec -Arguments @('/x', $productCode) -LogPath (Join-Path $OutputDirectory "$timestamp-uninstall.log")
    if (Test-Path -LiteralPath $installedExecutable) {
        throw '卸载后应用本体仍然存在。'
    }
    if (-not (Test-Path -LiteralPath $sentinel)) {
        throw '卸载错误删除了用户数据哨兵。'
    }
    if (Test-Path -LiteralPath 'HKCU:\Software\Classes\quicknote') {
        throw '卸载后 quicknote 协议仍然存在。'
    }
    if (Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'QuickNote' -ErrorAction SilentlyContinue) {
        throw '卸载后登录启动值仍然存在。'
    }

    $result.lifecycle_checks = 'passed'
    $result.lifecycle_logs = Get-ChildItem -LiteralPath $OutputDirectory -Filter "$timestamp-*.log" |
        Select-Object -ExpandProperty FullName
}

$outputPath = Join-Path $OutputDirectory ('installer-validation-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
$result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $outputPath -Encoding utf8
$result | ConvertTo-Json -Depth 6
Write-Output "Result: $outputPath"
}
finally {
    if ($msi) {
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($msi.database) | Out-Null
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($msi.installer) | Out-Null
    }
    # 只删除本脚本在系统临时目录中以随机 GUID 创建的目录。
    $resolvedValidationStage = [IO.Path]::GetFullPath($validationStage)
    if ($resolvedValidationStage.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedValidationStage -Recurse -Force
    }
}
