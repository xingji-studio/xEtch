param([string]$Image = "")
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$root = Split-Path $PSScriptRoot -Parent
$candidates = @(
    (Join-Path $root "target/xetch/windows-x64/release/gui/xEtch.Windows.exe"),
    (Join-Path $root "target/gui/windows/bin/x64/Release/net8.0-windows10.0.19041.0/win-x64/xEtch.Windows.exe")
)
$binary = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $binary) { throw "Build the GUI first: .\xetch.cmd build" }
$arguments = @()
if ($Image) { $arguments = @("--image", (Quote-Argument (Resolve-Path $Image).Path)) }
$process = if ($arguments.Count) { Start-Process -FilePath $binary -ArgumentList $arguments -PassThru } else { Start-Process -FilePath $binary -PassThru }
Write-Host "Started xEtch (PID $($process.Id))"
