# Authenticode signing command for Nugget's Windows bundles (Tauri `bundle.windows.signCommand`).
#
# Tauri runs this once per file it signs: the app exe (again for each bundle type after patching
# it), the `nuggetd.exe` sidecar, the NSIS plugin DLLs and uninstaller, the WiX extension DLLs, and
# the finished NSIS setup exe and MSI. It replaces `%1` with the file path. The release workflow
# writes the config fragment (object form, so paths may contain spaces):
#
#   {"bundle":{"windows":{"signCommand":{"cmd":"pwsh","args":["-NoLogo","-NoProfile",
#     "-NonInteractive","-ExecutionPolicy","Bypass","-File","<repo>\packaging\windows\sign.ps1","%1"]}}}}
#
# Usage: sign.ps1 <file> [-DryRun]
#        sign.ps1 -InstallDlib <dir>      download the pinned Artifact Signing dlib; prints the DLL path
#
# Modes, chosen by environment variables (first match wins):
#
#   Azure Trusted Signing (renamed Azure Artifact Signing in 2025; same service and dlib)
#     AZURE_TRUSTED_SIGNING_ENDPOINT   e.g. https://eus.codesigning.azure.net (the account's region)
#     AZURE_TRUSTED_SIGNING_ACCOUNT    the signing account name
#     AZURE_TRUSTED_SIGNING_PROFILE    the certificate profile name
#     AZURE_TRUSTED_SIGNING_DLIB       path to bin\x64\Azure.CodeSigning.Dlib.dll (-InstallDlib)
#     Authentication (the dlib uses Azure.Identity's DefaultAzureCredential), first match wins:
#       AZURE_CLIENT_ID + AZURE_TENANT_ID + AZURE_CLIENT_SECRET  app registration secret (for
#         signing by hand or elsewhere; release.yml never passes one: a long-lived secret in the
#         build step would be readable by every crate's build script)
#       AZURE_CLIENT_ID + AZURE_TENANT_ID in a GitHub job with `id-token: write`: a fresh GitHub
#         OIDC token is requested for every file and handed to WorkloadIdentityCredential, so no
#         secret is stored and a long build never outlives the short-lived federated token
#       otherwise whatever `az login` left (AzureCliCredential), for signing by hand
#   Certificate in the store
#     NUGGET_SIGN_CERT_THUMBPRINT      SHA-1 thumbprint of a code-signing cert in CurrentUser\My
#                                      (an imported .pfx, a hardware token whose minidriver exposes
#                                      its cert there, or the throwaway cert of the dry-run proof)
#     NUGGET_SIGN_TIMESTAMP_URL        RFC 3161 server (default http://timestamp.digicert.com)
#
# Optional: NUGGET_SIGN_DESCRIPTION (default "Nugget"), NUGGET_SIGNTOOL (signtool.exe to use;
# TAURI_WINDOWS_SIGNTOOL_PATH is honoured too), NUGGET_SIGN_DRY_RUN=1 (same as -DryRun: print the
# signtool command line and metadata and exit 0 without signing or contacting any service),
# NUGGET_SIGN_LOG (append each signed file's path to this file).
#
# Exits non-zero on any error so Tauri, and the release, stop. Secrets are never printed: the
# command line holds only paths and identifiers, and the OIDC token goes to a file deleted after use.
#
# Other signing services. Each ships a signtool dlib/CSP or its own CLI; add a mode here that turns
# the file path into one command and keep the `exit` on failure:
#   - DigiCert KeyLocker: `smctl windows certsync` once (after `smctl` login via SM_HOST, SM_API_KEY,
#     SM_CLIENT_CERT_FILE/_PASSWORD), then its KSP exposes the cert in the store: use the thumbprint
#     mode as is, or call `smctl sign --keypair-alias <alias> --input %1`.
#   - SSL.com eSigner: `CodeSignTool sign -username=... -password=... -totp_secret=...
#     -credential_id=... -input_file_path=%1 -override=true` (secrets from the environment).
#   - USB token (SafeNet, YubiKey): install its minidriver on a self-hosted runner; the cert then
#     appears in CurrentUser\My and the thumbprint mode works (the token PIN prompt must be
#     pre-authorised, e.g. SafeNet "single logon").
#
# Needs PowerShell 7 (`pwsh`), which GitHub's Windows runners have.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Position = 0)][string]$File,
    [switch]$DryRun,
    [string]$InstallDlib
)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# Pinned dlib: Microsoft.ArtifactSigning.Client (successor of Microsoft.Trusted.Signing.Client,
# whose versions are now unlisted). The SHA-256 is of the .nupkg from nuget.org.
$DlibPackage = 'microsoft.artifactsigning.client'
$DlibVersion = '1.0.146'
$DlibSha256 = '7761e5d8f31e54b764da21d33cd8b0a2940e1b05526c7dfcb5e94b0431426f70'
$TrustedSigningTimestamp = 'http://timestamp.acs.microsoft.com'

# Tauri shows a sign command's output only with -v, so failures also go to the job summary.
# Messages hold only paths and identifiers, never secrets.
function Fail([string]$msg) {
    [Console]::Error.WriteLine("sign.ps1: error: $msg")
    if ($env:GITHUB_STEP_SUMMARY) {
        try { Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value "- **Windows signing failed** (sign.ps1): $msg" } catch { }
    }
    exit 1
}
function Log([string]$msg) { Write-Host "sign.ps1: $msg" }

# ---- -InstallDlib -----------------------------------------------------------------------------

if ($InstallDlib) {
    $dest = [IO.Path]::GetFullPath($InstallDlib)
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    $nupkg = Join-Path $dest "$DlibPackage.$DlibVersion.zip"
    $url = "https://api.nuget.org/v3-flatcontainer/$DlibPackage/$DlibVersion/$DlibPackage.$DlibVersion.nupkg"
    Log "downloading $DlibPackage $DlibVersion"
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $url -OutFile $nupkg -UseBasicParsing -MaximumRetryCount 3 -RetryIntervalSec 5
    $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $nupkg).Hash.ToLowerInvariant()
    if ($got -ne $DlibSha256) { Fail "checksum mismatch for $DlibPackage $DlibVersion`: got $got, want $DlibSha256" }
    $out = Join-Path $dest 'pkg'
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    Expand-Archive -LiteralPath $nupkg -DestinationPath $out
    Remove-Item -LiteralPath $nupkg
    $dll = Join-Path $out 'bin\x64\Azure.CodeSigning.Dlib.dll'
    if (-not (Test-Path -LiteralPath $dll)) { Fail "no bin\x64\Azure.CodeSigning.Dlib.dll in the package" }
    Write-Output $dll
    exit 0
}

# ---- arguments --------------------------------------------------------------------------------

if (-not $DryRun -and $env:NUGGET_SIGN_DRY_RUN -eq '1') { $DryRun = [switch]$true }
if (-not $File) { Fail 'usage: sign.ps1 <file> [-DryRun] | sign.ps1 -InstallDlib <dir>' }
if (-not (Test-Path -LiteralPath $File -PathType Leaf)) { Fail "no such file: $File" }
$File = (Resolve-Path -LiteralPath $File).Path

$description = if ($env:NUGGET_SIGN_DESCRIPTION) { $env:NUGGET_SIGN_DESCRIPTION } else { 'Nugget' }

# ---- signtool -----------------------------------------------------------------------------------

# The newest x64 signtool from the Windows 10/11 SDK (x64 to match the x64 dlib; it also runs on
# arm64 under emulation). An explicit path wins.
function Find-SignTool {
    foreach ($explicit in @($env:NUGGET_SIGNTOOL, $env:TAURI_WINDOWS_SIGNTOOL_PATH)) {
        if ($explicit) {
            if (Test-Path -LiteralPath $explicit -PathType Leaf) { return $explicit }
            Fail "signtool not found at $explicit"
        }
    }
    $roots = @()
    try {
        $kits = Get-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -Name KitsRoot10 -ErrorAction Stop
        $roots += $kits.KitsRoot10
    } catch { }
    if (${env:ProgramFiles(x86)}) { $roots += Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10' }
    foreach ($root in ($roots | Select-Object -Unique)) {
        $bin = Join-Path $root 'bin'
        if (-not (Test-Path -LiteralPath $bin)) { continue }
        $best = Get-ChildItem -LiteralPath $bin -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'x64\signtool.exe')) } |
            Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
        if ($best) { return (Join-Path $best.FullName 'x64\signtool.exe') }
        $flat = Join-Path $bin 'x64\signtool.exe' # SDKs before 10.0.15063
        if (Test-Path -LiteralPath $flat) { return $flat }
    }
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    Fail 'signtool.exe not found: install the Windows SDK or set NUGGET_SIGNTOOL'
}

function Format-CommandLine([string]$exe, [string[]]$argv) {
    (@($exe) + $argv | ForEach-Object { if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
}

# ---- mode ---------------------------------------------------------------------------------------

$azure = @($env:AZURE_TRUSTED_SIGNING_ENDPOINT, $env:AZURE_TRUSTED_SIGNING_ACCOUNT, $env:AZURE_TRUSTED_SIGNING_PROFILE)
$azureSet = @($azure | Where-Object { $_ }).Count
$metadata = $null

if ($azureSet -gt 0) {
    if ($azureSet -lt 3) { Fail 'AZURE_TRUSTED_SIGNING_ENDPOINT, _ACCOUNT and _PROFILE must all be set' }
    $endpoint = $env:AZURE_TRUSTED_SIGNING_ENDPOINT.Trim()
    # The dlib only accepts its own service hosts; checking here gives a clearer error.
    if ($endpoint -notmatch '^https://[a-z0-9-]+\.codesigning\.azure\.net/?$') {
        Fail "AZURE_TRUSTED_SIGNING_ENDPOINT must look like https://<region>.codesigning.azure.net, got '$endpoint'"
    }
    $dlib = $env:AZURE_TRUSTED_SIGNING_DLIB
    if (-not $dlib) { Fail 'AZURE_TRUSTED_SIGNING_DLIB is not set (sign.ps1 -InstallDlib <dir> prints it)' }
    if (-not (Test-Path -LiteralPath $dlib -PathType Leaf)) { Fail "dlib not found: $dlib" }

    $all = 'EnvironmentCredential', 'WorkloadIdentityCredential', 'ManagedIdentityCredential',
        'SharedTokenCacheCredential', 'VisualStudioCredential', 'VisualStudioCodeCredential',
        'AzureCliCredential', 'AzurePowerShellCredential', 'AzureDeveloperCliCredential',
        'InteractiveBrowserCredential'
    $haveApp = [bool]($env:AZURE_CLIENT_ID -and $env:AZURE_TENANT_ID)
    if ($haveApp -and $env:AZURE_CLIENT_SECRET) {
        $auth = 'client secret (EnvironmentCredential)'; $keep = 'EnvironmentCredential'
    } elseif ($haveApp -and $env:ACTIONS_ID_TOKEN_REQUEST_URL -and $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN) {
        $auth = 'GitHub OIDC federated token (WorkloadIdentityCredential)'; $keep = 'WorkloadIdentityCredential'
    } elseif ($env:AZURE_CLIENT_SECRET -or $env:AZURE_CLIENT_ID) {
        Fail 'AZURE_CLIENT_ID and AZURE_TENANT_ID are both needed (with AZURE_CLIENT_SECRET, or `id-token: write` for OIDC)'
    } else {
        $auth = 'Azure CLI login (AzureCliCredential)'; $keep = 'AzureCliCredential'
    }
    Log "mode: Azure Trusted Signing, account $($env:AZURE_TRUSTED_SIGNING_ACCOUNT), profile $($env:AZURE_TRUSTED_SIGNING_PROFILE), auth: $auth"

    $metadata = [ordered]@{
        Endpoint               = $endpoint
        CodeSigningAccountName = $env:AZURE_TRUSTED_SIGNING_ACCOUNT.Trim()
        CertificateProfileName = $env:AZURE_TRUSTED_SIGNING_PROFILE.Trim()
        ExcludeCredentials     = @($all | Where-Object { $_ -ne $keep })
    }
    if ($env:GITHUB_RUN_ID) { $metadata.CorrelationId = "github-$($env:GITHUB_REPOSITORY)-$($env:GITHUB_RUN_ID)-$($env:GITHUB_RUN_ATTEMPT)" }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('nugget-sign-' + [Guid]::NewGuid().ToString('N'))
    $metadataPath = Join-Path $tmp 'metadata.json'
    $timestamp = $TrustedSigningTimestamp
    $signArgs = @('sign', '/v', '/fd', 'SHA256', '/tr', $timestamp, '/td', 'SHA256',
        '/d', $description, '/dlib', $dlib, '/dmdf', $metadataPath, $File)
} elseif ($env:NUGGET_SIGN_CERT_THUMBPRINT) {
    $thumb = ($env:NUGGET_SIGN_CERT_THUMBPRINT -replace '\s', '').ToUpperInvariant()
    if ($thumb -notmatch '^[0-9A-F]{40}$') { Fail 'NUGGET_SIGN_CERT_THUMBPRINT must be a 40-hex-digit SHA-1 thumbprint' }
    $timestamp = if ($env:NUGGET_SIGN_TIMESTAMP_URL) { $env:NUGGET_SIGN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    Log "mode: certificate $thumb in CurrentUser\My"
    $signArgs = @('sign', '/v', '/fd', 'SHA256', '/sha1', $thumb, '/s', 'My',
        '/tr', $timestamp, '/td', 'SHA256', '/d', $description, $File)
} else {
    Fail 'no signing mode: set AZURE_TRUSTED_SIGNING_ENDPOINT/_ACCOUNT/_PROFILE or NUGGET_SIGN_CERT_THUMBPRINT'
}

$signtool = Find-SignTool

if ($DryRun) {
    Log "dry run, would run: $(Format-CommandLine $signtool $signArgs)"
    if ($metadata) { Log "metadata.json: $($metadata | ConvertTo-Json -Compress)" }
    exit 0
}

# ---- sign -------------------------------------------------------------------------------------

$code = 1
try {
    if ($metadata) {
        New-Item -ItemType Directory -Force -Path $tmp | Out-Null
        $metadata | ConvertTo-Json | Set-Content -LiteralPath $metadataPath -Encoding utf8NoBOM
    }
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        if ($metadata -and $keep -eq 'WorkloadIdentityCredential') {
            # A fresh token per attempt: GitHub's OIDC tokens are short-lived, and a release build
            # signs its last file long after the job started.
            $req = $env:ACTIONS_ID_TOKEN_REQUEST_URL + '&audience=' + [Uri]::EscapeDataString('api://AzureADTokenExchange')
            $jwt = $null
            try {
                $resp = Invoke-RestMethod -Uri $req -Headers @{ Authorization = "bearer $($env:ACTIONS_ID_TOKEN_REQUEST_TOKEN)" }
                $prop = if ($resp) { $resp.PSObject.Properties['value'] } else { $null }
                if ($prop) { $jwt = [string]$prop.Value }
            } catch {
                Log "GitHub OIDC token request failed (attempt $attempt): $($_.Exception.Message)"
            }
            if (-not $jwt) {
                $code = 1
                if ($attempt -lt 3) { Start-Sleep -Seconds (5 * $attempt); continue }
                Fail 'the GitHub OIDC token request returned no token (does the job have `id-token: write`?)'
            }
            $tokenFile = Join-Path $tmp 'oidc-token'
            Set-Content -LiteralPath $tokenFile -Value $jwt -NoNewline -Encoding ascii
            $env:AZURE_FEDERATED_TOKEN_FILE = $tokenFile
            Log 'minted a GitHub OIDC token for Azure (audience api://AzureADTokenExchange)'
        }
        Log "signing $File (attempt $attempt)"
        & $signtool @signArgs
        $code = $LASTEXITCODE
        if ($code -eq 0) { break }
        Log "signtool exited with $code"
        if ($attempt -lt 3) { Start-Sleep -Seconds (5 * $attempt) }
    }
} finally {
    if ($metadata -and (Test-Path -LiteralPath $tmp)) { Remove-Item -Recurse -Force -LiteralPath $tmp }
}
if ($code -ne 0) { Fail "signtool failed for $File (exit $code)" }
Log "signed $File"
if ($env:NUGGET_SIGN_LOG) { Add-Content -LiteralPath $env:NUGGET_SIGN_LOG -Value $File }
exit 0
