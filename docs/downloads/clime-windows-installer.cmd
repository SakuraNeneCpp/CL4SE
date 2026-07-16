@echo off
setlocal
chcp 65001 >nul
title CLIME Windows Installer

if /I "%CLIME_INSTALLER_DRY_RUN%"=="1" goto run_installer

echo.
echo   CLIME Windows Installer
echo   -----------------------
echo   Downloading CLIME and enabling automatic startup...
echo.

:run_installer
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -Command "if ($env:CLIME_INSTALLER_DRY_RUN -ne '1') { $ErrorActionPreference='Stop'; $d=Join-Path $env:LOCALAPPDATA 'Programs\CLIME'; $e=Join-Path $d 'clime.exe'; New-Item -ItemType Directory -Force $d | Out-Null; if (-not (Test-Path -LiteralPath $e)) { Invoke-WebRequest 'https://github.com/SakuraNeneCpp/CLIME_CapsLockIMEcommit/releases/download/v1.0.0/clime-windows-x86_64.exe' -OutFile $e }; & $e doctor; if ($LASTEXITCODE -ne 0) { throw 'CLIME doctor failed' }; & $e install-autostart; if ($LASTEXITCODE -ne 0) { throw 'CLIME autostart setup failed' }; if (-not (Get-Process clime -ErrorAction SilentlyContinue)) { Start-Process -WindowStyle Hidden -FilePath $e -ArgumentList 'run' } }"

if errorlevel 1 goto installation_failed
if /I "%CLIME_INSTALLER_DRY_RUN%"=="1" exit /b 0

echo.
echo   Installation complete. CLIME is now running.
echo   This window can be closed.
echo.
pause
exit /b 0

:installation_failed
echo.
echo   Installation failed. Please keep this window open and check the message above.
echo.
pause
exit /b 1
