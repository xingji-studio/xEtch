param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [switch]$SmokeTests,
    [string]$OutputDirectory = ""
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$env:DOTNET_CLI_UI_LANGUAGE = "en-US"
$env:VSLANG = "1033"
$root = Split-Path $PSScriptRoot -Parent
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
if (-not (Test-Path $vswhere)) { throw "Visual Studio with MSBuild and Windows SDK is required." }
$paths = Invoke-Tool $vswhere @("-latest", "-products", "*", "-requires", "Microsoft.Component.MSBuild", "-find", "MSBuild\**\Bin\MSBuild.exe")
$msbuild = ($paths -split "`r?`n" | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1)
if (-not $msbuild) { throw "MSBuild was not found by vswhere." }
$arguments = @((Join-Path $root "apps/windows/xEtch.Windows.csproj"), "/restore", "/p:RestoreLockedMode=true", "/p:Configuration=$Configuration", "/p:Platform=x64", "/nologo", "/v:minimal")
if ($SmokeTests) { $arguments += "/p:XetchGuiSmokeTest=true" }
if ($OutputDirectory) { $arguments += "/p:OutputPath=$([IO.Path]::GetFullPath($OutputDirectory))\" }
Write-Host (Invoke-Tool $msbuild $arguments)
