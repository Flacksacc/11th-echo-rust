param(
    [string]$ConfigPath,
    [string]$BundlePath,
    [switch]$SkipBuild,
    [switch]$ValidateOnly
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$buildScript = Join-Path $PSScriptRoot "build-installer.ps1"

if (-not $ConfigPath) {
    $ConfigPath = if ($env:ECHO_UPDATE_CONFIG) {
        $env:ECHO_UPDATE_CONFIG
    }
    else {
        Join-Path $PSScriptRoot "update-config.local.json"
    }
}
$ConfigPath = [IO.Path]::GetFullPath($ConfigPath)
if (-not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
    throw "Missing update configuration '$ConfigPath'."
}

$config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
if (-not $config.publish_host -or
    [string]$config.publish_host -notmatch '^[A-Za-z0-9._-]+$') {
    throw "publish_host must be a simple SSH host or alias containing only letters, numbers, dots, underscores, and hyphens."
}
$publishHost = [string]$config.publish_host

if (-not $config.publish_path) {
    throw "The update configuration is missing publish_path."
}
$publishPath = ([string]$config.publish_path).TrimEnd('/')
if (-not $publishPath.StartsWith('/var/www/', [StringComparison]::Ordinal) -or
    $publishPath -notmatch '^/[A-Za-z0-9._/-]+$' -or
    $publishPath -match '(^|/)\.\.(/|$)') {
    throw "publish_path must be a safe absolute directory below /var/www."
}

if (-not $SkipBuild) {
    Write-Host "Building the production installer and signed update bundle..."
    $previousUpdateConfig = $env:ECHO_UPDATE_CONFIG
    try {
        $env:ECHO_UPDATE_CONFIG = $ConfigPath
        & $buildScript
        if ($LASTEXITCODE -ne 0) {
            throw "The installer build failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        if ($null -eq $previousUpdateConfig) {
            Remove-Item Env:ECHO_UPDATE_CONFIG -ErrorAction SilentlyContinue
        }
        else {
            $env:ECHO_UPDATE_CONFIG = $previousUpdateConfig
        }
    }
}

if (-not $BundlePath) {
    $BundlePath = Join-Path $PSScriptRoot "output\update-bundle"
}
$BundlePath = [IO.Path]::GetFullPath($BundlePath)
if (-not (Test-Path -LiteralPath $BundlePath -PathType Container)) {
    throw "The update bundle does not exist: $BundlePath"
}

$manifestPath = Join-Path $BundlePath "manifest.json"
$signaturePath = Join-Path $BundlePath "manifest.json.minisig"
$hashesPath = Join-Path $BundlePath "SHA256SUMS.txt"
foreach ($requiredFile in @($manifestPath, $signaturePath, $hashesPath)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "The update bundle is incomplete. Missing: $requiredFile"
    }
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 1 -or $manifest.channel -ne "stable") {
    throw "Only schema-version 1 stable update manifests can be published."
}
if ([string]$manifest.version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') {
    throw "The manifest version must be a stable three-part semantic version."
}
$installerName = [string]$manifest.installer.file
if ($installerName -notmatch '^Echo-[0-9A-Za-z.+-]+-Setup\.exe$' -or
    [IO.Path]::GetFileName($installerName) -ne $installerName) {
    throw "The manifest contains an unsafe installer filename."
}
$installerPath = Join-Path $BundlePath $installerName
if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "The manifest installer is missing: $installerPath"
}

$installer = Get-Item -LiteralPath $installerPath
if ($installer.Length -ne [int64]$manifest.installer.size) {
    throw "Installer size mismatch. Manifest: $($manifest.installer.size); file: $($installer.Length)."
}
$actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $installerPath).Hash.ToLowerInvariant()
if ($actualHash -ne ([string]$manifest.installer.sha256).ToLowerInvariant()) {
    throw "Installer SHA-256 does not match manifest.json."
}
$hashLine = (Get-Content -LiteralPath $hashesPath -Raw).Trim()
if ($hashLine -ne "$actualHash  $installerName") {
    throw "SHA256SUMS.txt does not exactly describe the bundled installer."
}

if (-not $config.public_key_path) {
    throw "The update configuration is missing public_key_path."
}
$publicKeyPath = [IO.Path]::GetFullPath([string]$config.public_key_path)
if (-not (Test-Path -LiteralPath $publicKeyPath -PathType Leaf)) {
    throw "The Minisign public key does not exist: $publicKeyPath"
}
$minisign = Get-Command "minisign.exe" -ErrorAction SilentlyContinue
if (-not $minisign) {
    $minisign = Get-Command "minisign" -ErrorAction SilentlyContinue
}
if (-not $minisign) {
    throw "Minisign was not found on PATH."
}
& $minisign.Source -V -q -p $publicKeyPath -m $manifestPath -x $signaturePath
if ($LASTEXITCODE -ne 0) {
    throw "The update manifest signature is invalid. Nothing was uploaded."
}

$feedUri = $null
if (-not $config.feed_url -or
    -not [Uri]::TryCreate([string]$config.feed_url, [UriKind]::Absolute, [ref]$feedUri) -or
    $feedUri.Scheme -ne "https") {
    throw "feed_url must be an absolute HTTPS URL."
}
if (-not $feedUri.AbsolutePath.EndsWith('/manifest.json', [StringComparison]::OrdinalIgnoreCase)) {
    throw "feed_url must end with /manifest.json."
}

Write-Host "Validated signed Echo $($manifest.version) update bundle."
if ($ValidateOnly) {
    Write-Host "Validation-only mode complete. No server files were changed."
    return
}

$ssh = Get-Command "ssh.exe" -ErrorAction SilentlyContinue
$scp = Get-Command "scp.exe" -ErrorAction SilentlyContinue
if (-not $ssh -or -not $scp) {
    throw "OpenSSH ssh.exe and scp.exe must be available on PATH."
}

& $ssh.Source -o BatchMode=yes -o ConnectTimeout=10 $publishHost "sudo -n true"
if ($LASTEXITCODE -ne 0) {
    throw "Could not connect to SSH host '$publishHost' with non-interactive sudo access."
}
$remoteStage = (& $ssh.Source -o BatchMode=yes $publishHost "mktemp -d /tmp/echo-update.XXXXXX").Trim()
if ($LASTEXITCODE -ne 0 -or $remoteStage -notmatch '^/tmp/echo-update\.[A-Za-z0-9]+$') {
    throw "The server did not return a safe temporary staging path."
}

try {
    Push-Location $BundlePath
    try {
        & $scp.Source -q $installerName "manifest.json.minisig" "SHA256SUMS.txt" "manifest.json" "${publishHost}:$remoteStage/"
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to upload the update bundle to the staging directory."
        }
    }
    finally {
        Pop-Location
    }

    # Every interpolated remote value above is restricted to a conservative
    # filename/path character set before reaching this command.
    $remoteCommand = @(
        "set -eu",
        "cd $remoteStage",
        "sha256sum -c SHA256SUMS.txt",
        "sudo install -d -o root -g www-data -m 775 $publishPath",
        "sudo install -o root -g root -m 644 $installerName $publishPath/$installerName.incoming",
        "sudo install -o root -g root -m 644 manifest.json.minisig $publishPath/manifest.json.minisig.incoming",
        "sudo install -o root -g root -m 644 SHA256SUMS.txt $publishPath/SHA256SUMS.txt.incoming",
        "sudo install -o root -g root -m 644 manifest.json $publishPath/manifest.json.incoming",
        "sudo mv -f $publishPath/$installerName.incoming $publishPath/$installerName",
        "sudo mv -f $publishPath/SHA256SUMS.txt.incoming $publishPath/SHA256SUMS.txt",
        "sudo mv -f $publishPath/manifest.json.minisig.incoming $publishPath/manifest.json.minisig",
        "sudo mv -f $publishPath/manifest.json.incoming $publishPath/manifest.json"
    ) -join "; "
    & $ssh.Source -o BatchMode=yes $publishHost $remoteCommand
    if ($LASTEXITCODE -ne 0) {
        throw "The server rejected the staged update. The public manifest was not intentionally advanced."
    }
}
finally {
    & $ssh.Source -o BatchMode=yes $publishHost "rm -f $remoteStage/$installerName $remoteStage/manifest.json.minisig $remoteStage/SHA256SUMS.txt $remoteStage/manifest.json; rmdir $remoteStage 2>/dev/null || true" | Out-Null
}

$installerUrl = [Uri]::new($feedUri, $installerName).AbsoluteUri
try {
    $publicManifest = Invoke-RestMethod -Uri $feedUri.AbsoluteUri -Headers @{ "Cache-Control" = "no-cache" }
    if ([string]$publicManifest.version -ne [string]$manifest.version -or
        [string]$publicManifest.installer.sha256 -ne [string]$manifest.installer.sha256) {
        throw "The public manifest does not match the published bundle."
    }
    $publicInstaller = Invoke-WebRequest -Uri $installerUrl -Method Head -UseBasicParsing
    if ($publicInstaller.StatusCode -ne 200) {
        throw "The public installer returned HTTP $($publicInstaller.StatusCode)."
    }
}
catch {
    throw "The server deployment completed, but public HTTPS verification failed: $($_.Exception.Message)"
}
Write-Host "Published Echo $($manifest.version) successfully."
Write-Host "Manifest: $($feedUri.AbsoluteUri)"
Write-Host "Installer: $installerUrl"
