[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Root, [switch]$CodexOnly)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$managedRoot = [IO.Path]::GetFullPath($Root)
New-Item -ItemType Directory -Force -Path $managedRoot | Out-Null
$sourceRoot = Join-Path $managedRoot 'source'
$hermesHome = Join-Path $managedRoot 'home'
$codexRoot = Join-Path $managedRoot 'codex-bin'
$hermesCommit = '8620effe538c294fa3bd923d93931854a8c8a4c9'
$bootstrapHash = 'e8b0e5d378d005a733e2750af2727f0bba86378ac9bdb79fc45d06dfe7f36c97'

function Get-VerifiedArtifact([string]$Url, [string]$Hash, [string]$Destination) {
    if (-not $Url.StartsWith('https://')) { throw 'Download requires HTTPS' }
    $partial = "$Destination.partial"
    Invoke-WebRequest -Uri $Url -OutFile $partial -UseBasicParsing
    if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Hash) {
        Remove-Item -LiteralPath $partial -Force
        throw 'Downloaded component checksum does not match its pinned release'
    }
    Move-Item -LiteralPath $partial -Destination $Destination -Force
}

# The upstream installer provisions its own Git, Python and dependency runtime.
# Use individual stages: publishing user PATH and installing a gateway service
# belong to Hermes' standalone installer, not an Echo-owned private component.
if (-not $CodexOnly) {
    $bootstrap = Join-Path $managedRoot 'upstream-install.ps1'
    Get-VerifiedArtifact "https://raw.githubusercontent.com/NousResearch/hermes-agent/$hermesCommit/scripts/install.ps1" $bootstrapHash $bootstrap
    $env:HERMES_HOME = $hermesHome
    $env:UV_PYTHON_INSTALL_DIR = Join-Path $managedRoot 'bootstrap-python'
    foreach ($stage in @('prerequisites', 'repository', 'python-deps', 'config')) {
        Write-Output "Hermes: $stage"
        & $bootstrap -Stage $stage -InstallDir $sourceRoot -HermesHome $hermesHome -Commit $hermesCommit -NonInteractive -SkipBrowser -SkipComputerUse -Json
        if ($LASTEXITCODE -ne 0) { throw "Hermes stage failed: $stage" }
    }
}

# Release hashes published by GitHub for OpenAI's 0.160.1 Windows x64 assets.
# Keep helper executables beside Codex so Windows sandbox/runtime discovery
# follows the upstream layout. No npm or separate developer install is needed.
$assets = @{
    'codex' = '25d308d675f366ee1e825d96755779029e95ba43eca41eae5d960698eaae9913'
    'codex-app-server' = '69439abb02f466c54a924a2ec3a6ca0bbfe44da698cad2988201db7f0d489912'
    'codex-command-runner' = '4c616571abe0e75cb558c9dbb61074e2000a346a6222010860fed633ab13b828'
    'codex-windows-sandbox-service' = 'ec6a05d18acb25de6f69b05e3867b9659b2ce9ecd2289358df37d1f572d29626'
    'codex-windows-sandbox-setup' = 'fb59660545ab55f21e51b645a87036eb18ebc3d0c6a0c9b2e6bcca456656c35a'
    'codex-code-mode-host' = '0aed582954478b8058ad80db892abff11a22872ebd91985df73b856509d3bec7'
}
New-Item -ItemType Directory -Force -Path $codexRoot | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($name in $assets.Keys) {
    Write-Output "Codex: $name"
    $archivePath = Join-Path $managedRoot "$name.zip"
    Get-VerifiedArtifact "https://github.com/openai/codex/releases/download/rust-v0.160.1/$name-x86_64-pc-windows-msvc.exe.zip" $assets[$name] $archivePath
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        # Only the expected executable is extracted. Reject every path, link,
        # nested archive or extra entry rather than trusting archive filenames.
        $expectedName = "$name-x86_64-pc-windows-msvc.exe"
        if ($name -eq 'codex') {
            # The main release is a runtime bundle. Preserve helper/resource
            # discovery while checking every resolved target before extraction.
            if (-not ($archive.Entries | Where-Object FullName -eq $expectedName)) { throw 'Codex executable is missing' }
            foreach ($entry in $archive.Entries) {
                if ($entry.FullName.Contains('\') -or $entry.FullName.StartsWith('/') -or $entry.FullName.Contains(':') -or
                    (($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'Unsafe archive entry' }
                $entryTarget = [IO.Path]::GetFullPath((Join-Path $codexRoot $entry.FullName))
                if (-not $entryTarget.StartsWith($codexRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Archive path escapes component directory' }
                if ($entry.FullName.EndsWith('/')) { New-Item -ItemType Directory -Force -Path $entryTarget | Out-Null }
                else {
                    New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($entryTarget)) | Out-Null
                    [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $entryTarget, $true)
                }
            }
            Move-Item -LiteralPath (Join-Path $codexRoot $expectedName) -Destination (Join-Path $codexRoot 'codex.exe') -Force
        } elseif ($archive.Entries.Count -ne 1 -or $archive.Entries[0].FullName -ne $expectedName) {
            throw 'Unexpected Codex archive layout'
        } else {
            [IO.Compression.ZipFileExtensions]::ExtractToFile($archive.Entries[0], (Join-Path $codexRoot "$name.exe"), $true)
        }
    } finally { $archive.Dispose() }
    Remove-Item -LiteralPath $archivePath -Force
}
New-Item -ItemType Directory -Force -Path (Join-Path $managedRoot 'codex-home') | Out-Null
Write-Output 'Managed Hermes and Codex components installed'
