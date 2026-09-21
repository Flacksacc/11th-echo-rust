$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $projectRoot "Cargo.toml"
$installerScript = Join-Path $PSScriptRoot "11th-echo.iss"
$noticeScript = Join-Path $PSScriptRoot "generate-rust-notices.ps1"
$updateConfigPath = if ($env:ECHO_UPDATE_CONFIG) {
    [IO.Path]::GetFullPath($env:ECHO_UPDATE_CONFIG)
}
else {
    Join-Path $PSScriptRoot "update-config.local.json"
}
$sherpaArchiveName = "sherpa-onnx-v1.13.4-win-x64-static-MT-Release-lib.tar.bz2"
$sherpaArchiveSha256 = "D81BD1D25112540862D2387072E76B2B6843EF962918D6B5C7DB5A19C6276B4C"
$sherpaArchiveUrl = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.4/$sherpaArchiveName"
$releaseInputDir = Join-Path $projectRoot "target\release-inputs"
$sherpaArchivePath = Join-Path $releaseInputDir $sherpaArchiveName
$sherpaCacheRoot = [IO.Path]::GetFullPath(
    (Join-Path $projectRoot "target\sherpa-onnx-prebuilt")
)
$targetRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot "target"))
if (-not $sherpaCacheRoot.StartsWith(
    $targetRoot + [IO.Path]::DirectorySeparatorChar,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw "Resolved sherpa-onnx cache is outside the Cargo target directory."
}

New-Item -ItemType Directory -Path $releaseInputDir -Force | Out-Null
if (-not (Test-Path -LiteralPath $sherpaArchivePath)) {
    $partialArchive = "$sherpaArchivePath.partial"
    Remove-Item -LiteralPath $partialArchive -Force -ErrorAction SilentlyContinue
    Invoke-WebRequest -Uri $sherpaArchiveUrl -OutFile $partialArchive
    Move-Item -LiteralPath $partialArchive -Destination $sherpaArchivePath
}
$actualSherpaHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $sherpaArchivePath).Hash
if ($actualSherpaHash -ne $sherpaArchiveSha256) {
    throw "Sherpa ONNX archive hash mismatch. Expected $sherpaArchiveSha256, got $actualSherpaHash."
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Could not read the package version from Cargo.toml."
}
$appVersion = $Matches[1]

if (-not (Test-Path -LiteralPath $updateConfigPath -PathType Leaf)) {
    throw "Missing update configuration '$updateConfigPath'. Copy installer\update-config.example.json to installer\update-config.local.json and replace every placeholder."
}
$updateConfig = Get-Content -LiteralPath $updateConfigPath -Raw | ConvertFrom-Json
if ($env:ECHO_UPDATE_FEED_URL) {
    $updateConfig.feed_url = $env:ECHO_UPDATE_FEED_URL
}
if ($env:ECHO_UPDATE_PUBLIC_KEY_PATH) {
    $updateConfig.public_key_path = $env:ECHO_UPDATE_PUBLIC_KEY_PATH
}
if ($env:ECHO_UPDATE_SECRET_KEY_PATH) {
    $updateConfig.secret_key_path = $env:ECHO_UPDATE_SECRET_KEY_PATH
}
if ($env:ECHO_UPDATE_RELEASE_NOTES_PATH) {
    $updateConfig.release_notes_path = $env:ECHO_UPDATE_RELEASE_NOTES_PATH
}
if (-not $updateConfig.feed_url) {
    throw "Update configuration is missing feed_url."
}
$feedUri = $null
if (-not [Uri]::TryCreate([string]$updateConfig.feed_url, [UriKind]::Absolute, [ref]$feedUri) -or
    $feedUri.Scheme -ne "https" -or
    -not $feedUri.Host -or
    $feedUri.Host.EndsWith(".invalid")) {
    throw "feed_url must be a non-placeholder absolute HTTPS URL."
}
if (-not $feedUri.AbsolutePath.EndsWith("/manifest.json", [StringComparison]::OrdinalIgnoreCase)) {
    throw "feed_url must end with /manifest.json."
}
foreach ($property in @("public_key_path", "secret_key_path")) {
    if (-not $updateConfig.$property) {
        throw "Update configuration is missing $property."
    }
    $resolved = [IO.Path]::GetFullPath([string]$updateConfig.$property)
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "Update configuration file '$property' does not exist: $resolved"
    }
    $updateConfig.$property = $resolved
}
$publicKeyText = (Get-Content -LiteralPath $updateConfig.public_key_path -Raw).Trim()
$publicKeyPayload = @(
    $publicKeyText -split "`r?`n" |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ -and -not $_.StartsWith("untrusted comment:") }
) | Select-Object -First 1
if (-not $publicKeyPayload -or $publicKeyPayload -notmatch '^[A-Za-z0-9+/]{56}=*$') {
    throw "public_key_path does not contain a valid Minisign public key."
}
$minisign = Get-Command "minisign.exe" -ErrorAction SilentlyContinue
if (-not $minisign) {
    $minisign = Get-Command "minisign" -ErrorAction SilentlyContinue
}
if (-not $minisign) {
    throw "Minisign was not found. Install it from https://jedisct1.github.io/minisign/ and ensure minisign is on PATH."
}
& $noticeScript
if ($LASTEXITCODE -ne 0) {
    throw "Third-party notice generation failed."
}
$legacyInstaller = Join-Path $PSScriptRoot "output\11th-Echo-$appVersion-Setup.exe"
if (Test-Path -LiteralPath $legacyInstaller) {
    Remove-Item -LiteralPath $legacyInstaller -Force
}
$legacyBinary = Join-Path $projectRoot "target\release\eleventh_echo_rust.exe"
if (Test-Path -LiteralPath $legacyBinary) {
    Remove-Item -LiteralPath $legacyBinary -Force
}

Push-Location $projectRoot
$previousSherpaArchiveDir = $env:SHERPA_ONNX_ARCHIVE_DIR
$previousUpdateFeedUrl = $env:ECHO_UPDATE_FEED_URL
$previousUpdatePublicKey = $env:ECHO_UPDATE_PUBLIC_KEY
try {
    # sherpa-onnx-sys otherwise trusts an already-extracted target cache. Force
    # every production build to extract from the repository-pinned archive.
    $sherpaExtractedDir = Join-Path $sherpaCacheRoot (
        $sherpaArchiveName -replace '\.tar\.bz2$', ''
    )
    $sherpaCachedArchive = Join-Path $sherpaCacheRoot $sherpaArchiveName
    if (Test-Path -LiteralPath $sherpaExtractedDir) {
        Remove-Item -LiteralPath $sherpaExtractedDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $sherpaCachedArchive) {
        Remove-Item -LiteralPath $sherpaCachedArchive -Force
    }
    $env:SHERPA_ONNX_ARCHIVE_DIR = $releaseInputDir
    $env:ECHO_UPDATE_FEED_URL = $feedUri.AbsoluteUri
    $env:ECHO_UPDATE_PUBLIC_KEY = $publicKeyText
    cargo clean -p sherpa-onnx-sys --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Could not clean sherpa-onnx-sys before the verified build."
    }
    cargo build --release --locked --bin echo --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo build failed with exit code $LASTEXITCODE."
    }

    $iscc = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
    if (-not $iscc) {
        $knownPaths = @(
            @(
                (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
                (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
                (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe")
            ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
        )
        if ($knownPaths.Count -gt 0) {
            $iscc = Get-Item -LiteralPath $knownPaths[0]
        }
    }

    if (-not $iscc) {
        throw "Inno Setup 6 was not found. Install it from https://jrsoftware.org/isdl.php and run this script again."
    }

    & $iscc.FullName "/DAppVersion=$appVersion" $installerScript
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup failed with exit code $LASTEXITCODE."
    }

    $installerPath = Join-Path $PSScriptRoot "output\Echo-$appVersion-Setup.exe"
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
        throw "Inno Setup completed but the expected installer is missing: $installerPath"
    }
    if ([bool]$updateConfig.authenticode_required) {
        foreach ($signedFile in @(
            (Join-Path $projectRoot "target\release\echo.exe"),
            $installerPath
        )) {
            $signature = Get-AuthenticodeSignature -LiteralPath $signedFile
            if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
                throw "authenticode_required is true, but '$signedFile' does not have a valid trusted Authenticode signature (status: $($signature.Status))."
            }
        }
    }
    $bundleDir = Join-Path $PSScriptRoot "output\update-bundle"
    if (Test-Path -LiteralPath $bundleDir) {
        Remove-Item -LiteralPath $bundleDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $bundleDir -Force | Out-Null
    $bundleInstaller = Join-Path $bundleDir ([IO.Path]::GetFileName($installerPath))
    Copy-Item -LiteralPath $installerPath -Destination $bundleInstaller -Force
    $installerInfo = Get-Item -LiteralPath $bundleInstaller
    $installerHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $bundleInstaller).Hash.ToLowerInvariant()
    $releaseNotes = $null
    if ($updateConfig.release_notes_path) {
        $notesPath = [IO.Path]::GetFullPath([string]$updateConfig.release_notes_path)
        if (-not (Test-Path -LiteralPath $notesPath -PathType Leaf)) {
            throw "release_notes_path does not exist: $notesPath"
        }
        $releaseNotes = (Get-Content -LiteralPath $notesPath -Raw).Trim()
        if ([Text.Encoding]::UTF8.GetByteCount($releaseNotes) -gt 20480) {
            throw "Release notes exceed the 20 KiB manifest limit."
        }
    }
    $manifestObject = [ordered]@{
        schema_version = 1
        channel = "stable"
        version = $appVersion
        published_at = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
        release_notes = $releaseNotes
        installer = [ordered]@{
            file = $installerInfo.Name
            size = $installerInfo.Length
            sha256 = $installerHash
        }
        authenticode_required = [bool]$updateConfig.authenticode_required
    }
    $manifestOutput = Join-Path $bundleDir "manifest.json"
    $manifestJson = $manifestObject | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText($manifestOutput, $manifestJson + "`n", [Text.UTF8Encoding]::new($false))
    $signatureOutput = "$manifestOutput.minisig"
    Remove-Item -LiteralPath $signatureOutput -Force -ErrorAction SilentlyContinue
    & $minisign.Source -S -s $updateConfig.secret_key_path -m $manifestOutput -x $signatureOutput
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $signatureOutput -PathType Leaf)) {
        throw "Minisign failed to sign the update manifest."
    }
    $hashOutput = Join-Path $bundleDir "SHA256SUMS.txt"
    [IO.File]::WriteAllText(
        $hashOutput,
        "$installerHash  $($installerInfo.Name)`n",
        [Text.UTF8Encoding]::new($false)
    )
}
finally {
    if ($null -eq $previousSherpaArchiveDir) {
        Remove-Item Env:SHERPA_ONNX_ARCHIVE_DIR -ErrorAction SilentlyContinue
    }
    else {
        $env:SHERPA_ONNX_ARCHIVE_DIR = $previousSherpaArchiveDir
    }
    if ($null -eq $previousUpdateFeedUrl) {
        Remove-Item Env:ECHO_UPDATE_FEED_URL -ErrorAction SilentlyContinue
    }
    else {
        $env:ECHO_UPDATE_FEED_URL = $previousUpdateFeedUrl
    }
    if ($null -eq $previousUpdatePublicKey) {
        Remove-Item Env:ECHO_UPDATE_PUBLIC_KEY -ErrorAction SilentlyContinue
    }
    else {
        $env:ECHO_UPDATE_PUBLIC_KEY = $previousUpdatePublicKey
    }
    Pop-Location
}

Write-Host "Installer and signed upload bundle created in installer\output. Upload the installer and manifest.json.minisig first, then upload manifest.json last."
