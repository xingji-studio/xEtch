param(
    [Parameter(Mandatory=$true)][string]$Package,
    [Parameter(Mandatory=$true)][string]$Directory,
    [Parameter(Mandatory=$true)][string]$Output,
    [string]$GuiTestBuild = ""
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$Package = (Resolve-Path $Package).Path
$Directory = [IO.Path]::GetFullPath($Directory)
if (Test-Path $Directory) { throw "Use a new validation directory." }
[IO.Directory]::CreateDirectory($Directory) | Out-Null
$relocated = Join-Path $Directory ("portable folder " + [char]0x4e2d + [char]0x6587)
Copy-Item -LiteralPath $Package -Destination $relocated -Recurse
$backend = Join-Path $relocated "xetch.exe"
$manifest = Get-Content -Raw -LiteralPath (Join-Path $relocated "xetch-package.json") | ConvertFrom-Json
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Name) { if (-not $Condition) { throw $Name }; $checks.Add($Name) }
function Invoke-XetchPackage([string[]]$Arguments) { (Invoke-Tool $backend (@("--json") + $Arguments) -WorkingDirectory $Directory) | ConvertFrom-Json }
$report = @{schema_version=1;status="running";platform="native Windows x64";checks=$checks}
try {
    foreach ($name in @("XETCH_EXT4_ENGINE", "XETCH_EXT4_WORKER", "XETCH_GUI", "XETCH_BACKEND", "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON", "XETCH_NTFS_WORKER", "QEMU_MODULE_DIR", "DOTNET_ROOT")) {
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $env:DOTNET_MULTILEVEL_LOOKUP = "0"
    $env:XETCH_WORKSPACE_ROOT = Join-Path $Directory "gui-sessions"
    $capabilities = Invoke-XetchPackage @("doctor")
    Check (@($capabilities.tools | Where-Object {-not $_.available}).Count -eq 0) "bundled_qemu_without_PATH_or_development_overrides"
    Check ($capabilities.capabilities.ntfs_write -eq $manifest.ntfs_worker) "bundled_ntfs_worker_matches_package"
    $devices = Invoke-XetchPackage @("device", "list")
    Check ($devices.read_only -and -not $devices.write_enabled -and @($devices.devices | Where-Object { $_.protection.write_allowed }).Count -eq 0) "relocated_read_only_devices_without_development_environment"
    $image = Join-Path $Directory "source.img"
    $workspace = Join-Path $Directory "session"
    $payload = Join-Path $Directory "payload.txt"
    [IO.File]::WriteAllText($payload, "portable native Windows content`n", $utf8)
    Invoke-XetchPackage @("--experimental-filesystems", "create", "--output", $image, "--size", "64MiB", "--table", "gpt", "--filesystem", "ext4") | Out-Null
    Invoke-XetchPackage @("workspace", "create", $image, "--workspace", $workspace) | Out-Null
    if ($manifest.ext4_worker) { $env:XETCH_EXT4_ENGINE = "lwext4" }
    Invoke-XetchPackage @("--experimental-filesystems", "file", "put", "--workspace", $workspace, "--source", $payload, "--destination", "/data.txt") | Out-Null
    $saved = Join-Path $Directory "edited.vmdk"
    $readback = Join-Path $Directory "readback.txt"
    Invoke-XetchPackage @("apply", "--workspace", $workspace, "--output", $saved) | Out-Null
    Invoke-XetchPackage @("file", "get", $saved, "--source", "/data.txt", "--destination", $readback) | Out-Null
    if ($manifest.ext4_worker) { Check $true "bundled_bsd_ext4_worker_selected_without_PATH" }
    Check ((Get-FileHash $payload).Hash -eq (Get-FileHash $readback).Hash) "create_ext4_edit_and_vmdk_readback_in_relocated_unicode_path"
    $ps = Join-Path $PSHOME "powershell.exe"
    Invoke-Tool $ps @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $relocated "check-deps.ps1"), "-Isolated") -WorkingDirectory $Directory | Out-Null
    Check $true "runtime_check_without_python_or_dotnet_on_PATH"
    $qemu = Join-Path $relocated "qemu/qemu-img.exe"
    Move-Item -LiteralPath $qemu -Destination ($qemu+".hidden")
    try {
        $diagnostic = Invoke-Tool $ps @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $relocated "check-deps.ps1")) -ExpectFailure -WorkingDirectory $Directory
        Check ($diagnostic -match "qemu-img") "missing_dependency_is_reported"
    } finally { Move-Item -LiteralPath ($qemu+".hidden") -Destination $qemu }
    if ($manifest.ext4_worker) {
        $worker = Join-Path $relocated "xetch-ext4-worker.exe"
        Move-Item -LiteralPath $worker -Destination ($worker+".hidden")
        try {
            $diagnostic = Invoke-Tool $ps @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $relocated "check-deps.ps1")) -ExpectFailure -WorkingDirectory $Directory
            Check ($diagnostic -match "xetch-ext4-worker") "missing_bsd_ext4_worker_is_reported"
        } finally { Move-Item -LiteralPath ($worker+".hidden") -Destination $worker }
    }
    if ($manifest.gui_self_contained) {
        $gui = Join-Path $relocated "gui/xEtch.Windows.exe"
        & (Join-Path $PSScriptRoot "windows_launcher_validate.ps1") -Backend $backend -Gui $gui -Directory (Join-Path $Directory "launcher") -Output (Join-Path $Directory "launcher-results.json")
        $launcherReport = Get-Content -Raw -LiteralPath (Join-Path $Directory "launcher-results.json") | ConvertFrom-Json
        if ($launcherReport.status -ne "pass") { throw $launcherReport.error }
        $checks.Add("no_argument_gui_startup_via_process_shell_cmd_powershell_and_shortcut")
        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = $gui; $start.WorkingDirectory = $Directory; $start.UseShellExecute = $false
        $start.RedirectStandardError = $true; $start.StandardErrorEncoding = $utf8
        $process = [Diagnostics.Process]::Start($start)
        $errorTask = $process.StandardError.ReadToEndAsync()
        try {
            for ($i=0; $i -lt 50 -and -not $process.HasExited -and $process.MainWindowHandle -eq 0; $i++) {
                Start-Sleep -Milliseconds 200; $process.Refresh()
            }
            if ($process.HasExited) { throw "GUI exited during startup ($($process.ExitCode)): $($errorTask.Result)" }
            Check ($process.MainWindowHandle -ne 0) "production_gui_window_starts_with_bundled_runtime"
        } finally {
            for ($i=0; $i -lt 75 -and -not $process.HasExited; $i++) {
                $process.CloseMainWindow() | Out-Null
                if ($process.WaitForExit(200)) { break }
            }
            if (-not $process.HasExited) { $process.Kill(); throw "Owned GUI test instance did not close." }
            $process.Dispose()
        }
        if ($GuiTestBuild) {
            # Only this disposable clone receives the integration variant.
            Get-ChildItem -LiteralPath $GuiTestBuild | Copy-Item -Destination (Join-Path $relocated "gui") -Recurse -Force
            & (Join-Path $PSScriptRoot "windows_gui_validate.ps1") -Binary $gui -Backend $backend -CreateImage -FileSystem ext4 -DiscoverBackend -Directory (Join-Path $Directory "gui-flow") -Output (Join-Path $Directory "gui-flow-results.json")
            Check $true "gui_automatically_finds_packaged_backend_and_qemu_and_edits_ext4"
        }
    }
    $report.status = "pass"
} catch { $report.status = "fail"; $report.error = $_.ToString(); Write-Host $report.error }
$path = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
[IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 8), $utf8)
if ($report.status -ne "pass") { exit 1 }
Write-Host "PASS portable Windows package, backend, GUI and runtime checks"
