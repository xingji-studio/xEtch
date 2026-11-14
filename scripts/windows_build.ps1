param(
    [ValidateSet("build", "doctor")][string]$Action = "build",
    [ValidateSet("backend", "frontend", "all")][string]$Components = "all",
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [ValidateSet("x64", "arm64")][string]$Architecture = "x64",
    [string]$OutputDirectory,
    [string]$CargoTargetDirectory,
    [string]$CargoHome = "",
    [string]$CargoExecutable = "",
    [switch]$Offline
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$root = Split-Path $PSScriptRoot -Parent
Set-Location -LiteralPath $root
if ($Components -ne "backend" -and $Architecture -ne "x64") { throw "The WinUI frontend currently supports x64 only." }
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
if (-not (Test-Path $vswhere)) { throw "Install Visual Studio Build Tools with Desktop development with C++ and Windows SDK." }
$install = (Invoke-Tool $vswhere @("-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath")).Trim()
if (-not $install) { throw "Visual C++ build tools were not found. Add the Desktop development with C++ workload." }
$sdk = Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10"
$sdkVersion = Get-ChildItem (Join-Path $sdk "Lib") -Directory -ErrorAction SilentlyContinue |
    Where-Object { (Test-Path (Join-Path $_.FullName "um/$Architecture/kernel32.lib")) -and (Test-Path (Join-Path $_.FullName "ucrt/$Architecture/ucrt.lib")) } |
    Sort-Object { [Version]$_.Name } -Descending | Select-Object -First 1 -ExpandProperty Name
if (-not $sdkVersion) { throw "Install a Windows 10/11 SDK with $Architecture libraries (kernel32.lib and ucrt.lib)." }
$cargo = if ($CargoExecutable) { $CargoExecutable } else { $env:XETCH_CARGO }
if (-not $cargo) {
    $found = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if ($found) { $cargo = $found.Source }
    else {
        $candidates = @((Join-Path $env:USERPROFILE ".cargo/bin/cargo.exe"), (Join-Path $env:USERPROFILE ".rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe"))
        $cargo = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
}
if ($Components -ne "frontend" -and (-not $cargo -or -not (Test-Path $cargo))) { throw "Install Rust (rustup, stable MSVC), or set XETCH_CARGO to cargo.exe." }
Write-Host "[OK] Visual C++ and Windows SDK: $install"
Write-Host "[OK] Windows SDK: $sdkVersion"
if ($Components -ne "frontend") { Write-Host "[OK] Rust: $cargo" }
if ($Components -ne "backend") {
    $msbuild = (Invoke-Tool $vswhere @("-latest", "-products", "*", "-requires", "Microsoft.Component.MSBuild", "-find", "MSBuild\**\Bin\MSBuild.exe")).Trim()
    if (-not $msbuild) { throw "MSBuild is required for the WinUI frontend." }
    Write-Host "[OK] WinUI MSBuild available"
}
if ($Action -eq "doctor") { return }
if (-not $OutputDirectory) { throw "OutputDirectory is required." }
$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($outputPath) | Out-Null
if ($Components -ne "frontend") {
    # Import the developer environment in-process. Never print its values:
    # the inherited environment may contain unrelated credentials.
    $vcvars = Join-Path $install "VC/Auxiliary/Build/vcvarsall.bat"
    $vcArch = if ($Architecture -eq "arm64") { "amd64_arm64" } else { "amd64" }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $env:ComSpec
    $start.Arguments = '/d /s /c "call "' + $vcvars + '" ' + $vcArch + ' >nul && set"'
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    $values = $process.StandardOutput.ReadToEnd(); $errors = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Visual C++ environment setup failed: $errors" }
    $process.Dispose()
    foreach ($line in ($values -split "`r?`n")) {
        $at = $line.IndexOf('=')
        if ($at -gt 0) { [Environment]::SetEnvironmentVariable($line.Substring(0,$at), $line.Substring($at+1), "Process") }
    }
    # Some VS installations expose C++ tools without registering their SDK in
    # the registry used by vcvarsall. Use the verified installed SDK libraries.
    $env:LIB = "$sdk\Lib\$sdkVersion\um\$Architecture;$sdk\Lib\$sdkVersion\ucrt\$Architecture;$env:LIB"
    $env:INCLUDE = "$sdk\Include\$sdkVersion\ucrt;$sdk\Include\$sdkVersion\shared;$sdk\Include\$sdkVersion\um;$sdk\Include\$sdkVersion\winrt;$env:INCLUDE"
    $env:WindowsSdkDir = "$sdk\"
    $env:WindowsSDKVersion = "$sdkVersion\"
    if ($CargoHome) { $env:CARGO_HOME = [IO.Path]::GetFullPath($CargoHome) }
    if ($CargoTargetDirectory) { $env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($CargoTargetDirectory) }
    $siblingRustc = Join-Path (Split-Path $cargo -Parent) "rustc.exe"
    if (Test-Path $siblingRustc) { $env:RUSTC = $siblingRustc }
    $target = if ($Architecture -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
    $arguments = @("build", "--locked", "-p", "xetch-cli")
    if ($Profile -eq "release") { $arguments += "--release" }
    # The supported x64 host path uses the normal Cargo cache. arm64 is an
    # explicit backend cross target and requires its rustup target installed.
    if ($Architecture -eq "arm64") { $arguments += @("--target", $target) }
    if ($Offline) { $arguments += "--offline" }
    Write-Host "Building Windows $Architecture backend ($Profile)..."
    Write-Host (Invoke-Tool $cargo $arguments)
    $targetPath = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
    if ($Architecture -eq "arm64") { $targetPath = Join-Path $targetPath $target }
    $cargoProfile = if ($Profile -eq "release") { "release" } else { "debug" }
    $backend = Join-Path $outputPath "backend"
    [IO.Directory]::CreateDirectory($backend) | Out-Null
    Copy-Item -LiteralPath (Join-Path $targetPath "$cargoProfile/xetch.exe") -Destination $backend -Force
}
if ($Components -ne "backend") {
    $gui = Join-Path $outputPath "gui"
    $configuration = if ($Profile -eq "release") { "Release" } else { "Debug" }
    Write-Host "Publishing WinUI frontend with its .NET and Windows App SDK runtimes..."
    & (Join-Path $PSScriptRoot "windows_build_gui.ps1") -Configuration $configuration -Publish -SelfContained -Offline:$Offline -OutputDirectory $gui
    $crt = Get-ChildItem (Join-Path $install "VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT") -Directory -ErrorAction SilentlyContinue | Sort-Object FullName -Descending | Select-Object -First 1
    if ($crt) { Get-ChildItem $crt.FullName -Filter *.dll | Copy-Item -Destination $gui -Force }
    $notices = Join-Path $outputPath "gui-licenses"
    [IO.Directory]::CreateDirectory($notices) | Out-Null
    $nuget = if ($env:NUGET_PACKAGES) { $env:NUGET_PACKAGES } else { Join-Path $env:USERPROFILE ".nuget/packages" }
    foreach ($package in @("microsoft.netcore.app.runtime.win-x64", "microsoft.windowsappsdk", "microsoft.windowsappsdk.runtime", "microsoft.windowsappsdk.winui")) {
        $folder = Join-Path $nuget $package
        if (Test-Path $folder) {
            foreach ($file in (Get-ChildItem $folder -Recurse -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|ThirdPartyNotices|THIRD-PARTY-NOTICES)' })) {
                $relative = $file.FullName.Substring($folder.Length + 1)
                $destination = Join-Path $notices "$package/$relative"
                [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
                Copy-Item -LiteralPath $file.FullName -Destination $destination -Force
            }
        }
    }
}
Write-Host "[OK] Windows build: $outputPath"
