[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Path,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$CertificateThumbprint,

    [ValidatePattern('^https://')]
    [string]$TimestampUrl = 'https://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Find-SignTool {
    # 优先使用最新 Windows SDK 的 x64 SignTool，避免意外调用旧版或 x86 工具。
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidate = Get-ChildItem -LiteralPath $sdkRoot -Filter signtool.exe -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object FullName -Match '\\x64\\signtool\.exe$' |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $candidate) {
        throw '未找到 Windows SDK x64 SignTool。'
    }
    $candidate.FullName
}

$normalizedThumbprint = $CertificateThumbprint.Replace(' ', '').ToUpperInvariant()
$certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$normalizedThumbprint" -ErrorAction SilentlyContinue
if (-not $certificate -or -not $certificate.HasPrivateKey) {
    throw '当前用户证书库中没有带私钥的指定代码签名证书。'
}
if ($certificate.NotAfter -le [DateTime]::Now) {
    throw '指定代码签名证书已过期。'
}
if ($certificate.NotBefore -gt [DateTime]::Now) {
    throw '指定代码签名证书尚未生效。'
}
$codeSigningOid = '1.3.6.1.5.5.7.3.3'
if ($certificate.EnhancedKeyUsageList.ObjectId.Value -notcontains $codeSigningOid) {
    throw '指定证书没有 Code Signing EKU。'
}

$signTool = Find-SignTool
$results = @()
foreach ($itemPath in $Path) {
    $resolvedPath = (Resolve-Path -LiteralPath $itemPath).ProviderPath
    $extension = [IO.Path]::GetExtension($resolvedPath).ToLowerInvariant()
    if ($extension -notin @('.exe', '.msi')) {
        throw "只允许签名 EXE 或 MSI：$resolvedPath"
    }

    & $signTool sign `
        /sha1 $normalizedThumbprint `
        /fd SHA256 `
        /tr $TimestampUrl `
        /td SHA256 `
        /d 'QuickNote' `
        /du 'https://github.com/eelevenn/quicknote' `
        $resolvedPath
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool 签名失败，退出码：$LASTEXITCODE"
    }

    # 发布候选必须同时通过 Authenticode 策略和 SignTool 完整验证。
    & $signTool verify /pa /all /v $resolvedPath
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool 验证失败，退出码：$LASTEXITCODE"
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $resolvedPath
    if ($signature.Status -ne 'Valid') {
        throw "Authenticode 状态不是 Valid：$($signature.Status)"
    }
    if (-not $signature.TimeStamperCertificate) {
        throw '签名没有可验证的 RFC 3161 时间戳证书。'
    }

    $results += [ordered]@{
        path = $resolvedPath
        bytes = (Get-Item -LiteralPath $resolvedPath).Length
        sha256 = (Get-FileHash -LiteralPath $resolvedPath -Algorithm SHA256).Hash.ToLowerInvariant()
        signer_subject = $signature.SignerCertificate.Subject
        signer_thumbprint = $signature.SignerCertificate.Thumbprint
        timestamp_subject = $signature.TimeStamperCertificate.Subject
    }
}

$results | ConvertTo-Json -Depth 4
