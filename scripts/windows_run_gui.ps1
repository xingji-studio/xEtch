param([string]$Image = "")
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$root = Split-Path $PSScriptRoot -Parent
$binary = Join-Path $root "target/gui/windows/bin/x64/Release/net8.0-windows10.0.19041.0/win-x64/xEtch.Windows.exe"
if (-not (Test-Path $binary)) { throw "Build the GUI first: ./scripts/windows_build_gui.ps1" }
$arguments = @()
if ($Image) { $arguments = @("--image", (Quote-Argument (Resolve-Path $Image).Path)) }
$process = if ($arguments.Count) { Start-Process -FilePath $binary -ArgumentList $arguments -PassThru } else { Start-Process -FilePath $binary -PassThru }
Write-Host "Started xEtch (PID $($process.Id))"
