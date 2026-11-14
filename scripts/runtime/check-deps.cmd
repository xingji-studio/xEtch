@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0check-deps.ps1" %*
exit /b %ERRORLEVEL%
