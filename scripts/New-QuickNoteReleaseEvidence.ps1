[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [string[]]$ArtifactPath = @(),

    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) {
    $timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $OutputDirectory = Join-Path $repositoryRoot "artifacts\release-evidence\$timestamp-$Version"
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$operatingSystem = Get-CimInstance -ClassName Win32_OperatingSystem
$computer = Get-CimInstance -ClassName Win32_ComputerSystem
$processors = @(Get-CimInstance -ClassName Win32_Processor)
$videoControllers = @(Get-CimInstance -ClassName Win32_VideoController)
$powerPlan = (& powercfg.exe /getactivescheme 2>&1 | Out-String).Trim()

function Get-OptionalRegistryValue {
    param([string]$Path, [string]$Name)

    # 未显式配置的桌面值使用系统默认，因此缺失应记录为 null 而不是中止采集。
    try {
        Get-ItemPropertyValue -LiteralPath $Path -Name $Name -ErrorAction Stop
    }
    catch {
        $null
    }
}

# 记录 VC Redist 清单，干净系统验收不能把开发机已有运行时当作应用前置条件。
$vcRuntimes = @(
    Get-ItemProperty `
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*', `
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*' `
        -ErrorAction SilentlyContinue |
        Where-Object {
            $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -like 'Microsoft Visual C++*'
        } |
        Select-Object DisplayName, DisplayVersion, Publisher |
        Sort-Object DisplayName, DisplayVersion -Unique
)

$artifacts = @()
foreach ($candidate in $ArtifactPath) {
    $resolved = (Resolve-Path -LiteralPath $candidate).ProviderPath
    $item = Get-Item -LiteralPath $resolved
    $signature = Get-AuthenticodeSignature -LiteralPath $resolved
    $artifacts += [ordered]@{
        path = $resolved
        bytes = $item.Length
        sha256 = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
        signature_status = $signature.Status.ToString()
        signer_subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
    }
}

$environment = [ordered]@{
    schema_version = 1
    captured_at = [DateTime]::UtcNow.ToString('o')
    version = $Version
    user = $identity.Name
    is_administrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    os = [ordered]@{
        caption = $operatingSystem.Caption
        version = $operatingSystem.Version
        build_number = $operatingSystem.BuildNumber
        architecture = $operatingSystem.OSArchitecture
    }
    hardware = [ordered]@{
        manufacturer = $computer.Manufacturer
        model = $computer.Model
        total_physical_memory_bytes = [int64]$computer.TotalPhysicalMemory
        logical_processors = [int]$computer.NumberOfLogicalProcessors
        processors = @($processors | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors)
        video_controllers = @($videoControllers | Select-Object Name, DriverVersion)
    }
    desktop = [ordered]@{
        log_pixels = Get-OptionalRegistryValue -Path 'HKCU:\Control Panel\Desktop' -Name 'LogPixels'
        high_contrast_flags = Get-OptionalRegistryValue -Path 'HKCU:\Control Panel\Accessibility\HighContrast' -Name 'Flags'
        apps_use_light_theme = Get-OptionalRegistryValue -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize' -Name 'AppsUseLightTheme'
    }
    power_plan = $powerPlan
    installed_vc_runtimes = $vcRuntimes
    artifacts = $artifacts
}
$environmentPath = Join-Path $OutputDirectory 'environment.json'
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $environmentPath -Encoding utf8

# 每行必须关联原始 JSON/CSV、MSI 日志、截图或视频；状态默认不得声称通过。
$matrix = @(
    [ordered]@{ id = 'install-clean-standard-user'; category = 'installer'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'repair-same-version'; category = 'installer'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'upgrade-preserves-state'; category = 'upgrade'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'upgrade-failure-rollback'; category = 'upgrade'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'upgrade-power-loss-recovery'; category = 'upgrade'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'migration-failure-rollback'; category = 'data'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'uninstall-preserves-user-data'; category = 'installer'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'reinstall-reprojects-reminders'; category = 'installer'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'authenticode-smartscreen-publisher'; category = 'signing'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'keyboard-narrator-high-contrast'; category = 'accessibility'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'scale-100-150-200'; category = 'accessibility'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'cold-hot-idle-performance'; category = 'performance'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'dataset-10000-notes-100mib'; category = 'performance'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'voice-issue-21-gates'; category = 'voice'; status = 'not-run'; evidence = ''; notes = '' },
    [ordered]@{ id = 'third-party-license-review'; category = 'legal'; status = 'not-run'; evidence = ''; notes = '' }
)
$matrixPath = Join-Path $OutputDirectory 'acceptance-matrix.csv'
$matrix | ForEach-Object { [pscustomobject]$_ } |
    Export-Csv -LiteralPath $matrixPath -NoTypeInformation -Encoding utf8

[ordered]@{
    evidence_directory = (Resolve-Path -LiteralPath $OutputDirectory).ProviderPath
    environment = $environmentPath
    acceptance_matrix = $matrixPath
} | ConvertTo-Json -Depth 4
