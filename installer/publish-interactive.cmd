@echo off
setlocal

rem Open an interactive PowerShell 7 window so Minisign can safely prompt for
rem the private-key password. The companion script writes a non-secret status
rem marker under target\ for release automation to inspect.
set "SCRIPT=%~dp0publish-interactive.ps1"
where pwsh.exe >nul 2>nul
if errorlevel 1 (
    echo PowerShell 7 ^(pwsh.exe^) is required to publish Echo.
    pause
    exit /b 1
)

start "Echo release publisher" pwsh.exe -NoExit -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT%"
