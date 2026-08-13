[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Resolve all inputs explicitly so IExpress never depends on the caller's directory.
$benchmarkRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
$application = (Resolve-Path (Join-Path $benchmarkRoot 'slint/target/release/quicknote-stack-slint.exe')).ProviderPath
$installerSource = (Resolve-Path (Join-Path $benchmarkRoot 'slint/installer')).ProviderPath
$outputDirectory = Join-Path $benchmarkRoot 'slint/target/installer'
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
$output = Join-Path $outputDirectory 'QuickNoteSlintSpike-Setup.exe'

# Stage immutable inputs locally because IExpress cannot reliably consume WSL UNC paths.
$stage = Join-Path $env:TEMP 'quicknote-slint-installer'
if (Test-Path -LiteralPath $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item -LiteralPath $application -Destination (Join-Path $stage 'quicknote-stack-slint.exe')
Copy-Item -LiteralPath (Join-Path $installerSource 'install.cmd') -Destination $stage
Copy-Item -LiteralPath (Join-Path $installerSource 'uninstall.cmd') -Destination $stage

# IExpress produces one compressed, silent, per-user setup executable.
$sed = @"
[Version]
Class=IEXPRESS
SEDVersion=3
[Options]
PackagePurpose=InstallApp
ShowInstallProgramWindow=0
HideExtractAnimation=1
UseLongFileName=1
InsideCompressed=0
CAB_FixedSize=0
CAB_ResvCodeSigning=0
RebootMode=N
InstallPrompt=
DisplayLicense=
FinishMessage=
TargetName=$stage\QuickNoteSlintSpike-Setup.exe
FriendlyName=QuickNote Slint Integration Spike
AppLaunched=install.cmd
PostInstallCmd=<None>
AdminQuietInstCmd=install.cmd
UserQuietInstCmd=install.cmd
SourceFiles=SourceFiles
[SourceFiles]
SourceFiles0=$stage\
[SourceFiles0]
%FILE0%=quicknote-stack-slint.exe
%FILE1%=install.cmd
%FILE2%=uninstall.cmd
[Strings]
FILE0=quicknote-stack-slint.exe
FILE1=install.cmd
FILE2=uninstall.cmd
"@
$sedPath = Join-Path $stage 'package.sed'
$sed | Set-Content -LiteralPath $sedPath -Encoding ASCII
$iexpress = Start-Process -FilePath "$env:SystemRoot\System32\iexpress.exe" -ArgumentList @('/N', '/Q', $sedPath) -Wait -PassThru
$stageOutput = Join-Path $stage 'QuickNoteSlintSpike-Setup.exe'
if ($iexpress.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $stageOutput)) {
    throw "IExpress failed to build the Slint installer (exit $($iexpress.ExitCode))."
}
Copy-Item -LiteralPath $stageOutput -Destination $output -Force
Unblock-File -LiteralPath $output
Write-Host "Built installer: $output"
