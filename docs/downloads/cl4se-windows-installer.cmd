@echo off
setlocal
chcp 65001 >nul
title CL4SE Windows Installer

if /I "%CL4SE_INSTALLER_DRY_RUN%"=="1" goto run_installer

echo.
echo   CL4SE Windows Installer
echo   -----------------------
echo   Downloading CL4SE and enabling automatic startup...
echo.

:run_installer
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -Command "if ($env:CL4SE_INSTALLER_DRY_RUN -ne '1') { $ErrorActionPreference='Stop'; $d=Join-Path $env:LOCALAPPDATA 'Programs\CL4SE'; $e=Join-Path $d 'cl4se.exe'; $n=$e+'.new'; New-Item -ItemType Directory -Force $d | Out-Null; Invoke-WebRequest 'https://github.com/SakuraNeneCpp/CL4SE/releases/download/v1.0.3/cl4se-windows-x86_64.exe' -OutFile $n; if (Test-Path -LiteralPath $e) { & $e stop; if ($LASTEXITCODE -ne 0) { throw 'CL4SE stop failed' } }; Move-Item -LiteralPath $n -Destination $e -Force; & $e doctor; if ($LASTEXITCODE -ne 0) { throw 'CL4SE doctor failed' }; & $e install-autostart; if ($LASTEXITCODE -ne 0) { throw 'CL4SE autostart setup failed' }; & $e start; if ($LASTEXITCODE -ne 0) { throw 'CL4SE background start failed' } }"

if errorlevel 1 goto installation_failed
if /I "%CL4SE_INSTALLER_DRY_RUN%"=="1" exit /b 0

echo.
echo   Installation complete. CL4SE is now running.
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
