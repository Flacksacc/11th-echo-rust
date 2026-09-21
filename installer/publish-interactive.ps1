$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
$statusPath = Join-Path $projectRoot "target\echo-publish-status.json"
$publishScript = Join-Path $PSScriptRoot "build-and-publish.ps1"

function Write-PublishStatus([string]$state, [string]$message) {
    $statusDirectory = Split-Path -Parent $statusPath
    [IO.Directory]::CreateDirectory($statusDirectory) | Out-Null
    $status = [ordered]@{
        state = $state
        updated_at = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
        message = $message
    } | ConvertTo-Json
    [IO.File]::WriteAllText($statusPath, $status + "`n", [Text.UTF8Encoding]::new($false))
}

Write-PublishStatus "running" "Building and signing the Echo release."
Write-Host "Echo release publisher"
Write-Host "Minisign will ask for the private-key password. The password is not saved or passed on the command line."

try {
    & $publishScript
    if ($LASTEXITCODE -ne 0) {
        throw "The release publisher exited with code $LASTEXITCODE."
    }
    Write-PublishStatus "published" "The signed release was published successfully."
    Write-Host "`nRelease published successfully."
}
catch {
    Write-PublishStatus "failed" "The release was not published. Review the interactive console for details."
    Write-Error $_
    exit 1
}
