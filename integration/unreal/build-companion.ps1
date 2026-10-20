param(
 [string]$EngineRoot = 'C:/Program Files/Epic Games/UE_5.8',
 [string]$ProjectPath = (Join-Path $PSScriptRoot 'EchoAvatar/EchoAvatar.uproject'),
 [string]$Output = (Join-Path $PSScriptRoot '../../target/unreal-avatar')
)
$ErrorActionPreference = 'Stop'
$project = (Resolve-Path -LiteralPath $ProjectPath).Path
$uat = Join-Path $EngineRoot 'Engine/Build/BatchFiles/RunUAT.bat'
if (-not (Test-Path -LiteralPath $uat)) { throw 'Choose an installed Unreal Engine 5.8.2 directory.' }
& $uat BuildCookRun "-project=$project" -noP4 -platform=Win64 -clientconfig=Development -build -cook '-map=/Engine/Maps/Entry' -stage -pak -archive "-archivedirectory=$Output" -unattended -utf8output -NoXGE
if ($LASTEXITCODE -ne 0) { throw "Unreal package failed ($LASTEXITCODE)." }
Write-Output "Packaged companion: $Output"
