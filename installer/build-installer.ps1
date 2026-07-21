$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $projectRoot "Cargo.toml"
$installerScript = Join-Path $PSScriptRoot "11th-echo.iss"
$manifest = Get-Content -LiteralPath $manifestPath -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Could not read the package version from Cargo.toml."
}
$appVersion = $Matches[1]
$legacyInstaller = Join-Path $PSScriptRoot "output\11th-Echo-$appVersion-Setup.exe"
if (Test-Path -LiteralPath $legacyInstaller) {
    Remove-Item -LiteralPath $legacyInstaller -Force
}
$legacyBinary = Join-Path $projectRoot "target\release\eleventh_echo_rust.exe"
if (Test-Path -LiteralPath $legacyBinary) {
    Remove-Item -LiteralPath $legacyBinary -Force
}

Push-Location $projectRoot
try {
    cargo build --release --locked --manifest-path $manifestPath
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
}
finally {
    Pop-Location
}

Write-Host "Installer created in installer\output."
