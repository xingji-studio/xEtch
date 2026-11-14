param([switch]$Isolated)
$ErrorActionPreference = "Stop"
if ($Isolated) {
    foreach ($name in @("XETCH_BACKEND", "XETCH_NTFS_WORKER", "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON", "QEMU_MODULE_DIR", "DOTNET_ROOT")) {
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $env:DOTNET_MULTILEVEL_LOOKUP = "0"
}
function Invoke-Backend([string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $PSScriptRoot "xetch.exe"
    $start.Arguments = $Arguments -join ' '
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = $start.StandardOutputEncoding
    $process = [Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(30000)) { $process.Kill(); throw "Backend dependency check timed out." }
    $text = $stdout.Result; $errorText = $stderr.Result; $code = $process.ExitCode
    $process.Dispose()
    if ($code -ne 0) { throw "Backend failed ($code): $text $errorText" }
    return $text
}
try {
    if (-not [Environment]::Is64BitOperatingSystem) { throw "A 64-bit Windows installation is required." }
    if ([Environment]::OSVersion.Version -lt [Version]"10.0.19041") { throw "Windows 10 2004 (19041) or later is required." }
    $manifest = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot "xetch-package.json") | ConvertFrom-Json
    foreach ($file in $manifest.files.PSObject.Properties.Name) {
        if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot $file) -PathType Leaf)) { throw "Missing packaged file: $file. Restore the complete folder." }
    }
    Write-Host (Invoke-Backend @("--version"))
    $diagnostics = Invoke-Backend @("--json", "doctor") | ConvertFrom-Json
    foreach ($tool in $diagnostics.tools) {
        if (-not $tool.available) { throw "$($tool.name) is unavailable: $($tool.error). Restore qemu/ and its DLLs, or correct XETCH_QEMU_IMG/XETCH_QEMU_STORAGE_DAEMON." }
    }
    Write-Host "[OK] Backend and bundled QEMU tools can start."
    if ($diagnostics.capabilities.ntfs_write) { Write-Host "[OK] NTFS write worker is ready." }
    elseif ($manifest.ntfs_worker) { throw "The bundled NTFS worker is missing, incompatible, or lacks a runtime DLL." }
    else { Write-Host "[NOTE] NTFS worker absent: NTFS is read-only." }
    if ($manifest.gui_self_contained) {
        foreach ($name in @("coreclr.dll", "hostfxr.dll", "xEtch.Windows.exe", "xEtch.Windows.pri")) {
            if (-not (Test-Path (Join-Path $PSScriptRoot "gui/$name"))) { throw "Missing GUI runtime: $name. Rebuild/package the self-contained frontend." }
        }
        foreach ($name in @("vcruntime140.dll", "msvcp140.dll")) {
            if (-not (Test-Path (Join-Path $PSScriptRoot "gui/$name")) -and -not (Test-Path (Join-Path ([Environment]::SystemDirectory) $name))) {
                throw "Missing $name. Install the Microsoft Visual C++ v14 x64 Redistributable: https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist"
            }
        }
        Write-Host "[OK] GUI includes .NET/Windows App SDK runtime files."
    }
    Write-Host "[OK] Runtime dependency check passed."
} catch {
    Write-Host "[ERROR] $($_.Exception.Message)"
    exit 1
}
