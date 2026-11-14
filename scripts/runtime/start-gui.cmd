@echo off
if not exist "%~dp0gui\xEtch.Windows.exe" (
  echo GUI is missing. Keep the complete package folder together.
  exit /b 1
)
start "xEtch" "%~dp0gui\xEtch.Windows.exe" %*
