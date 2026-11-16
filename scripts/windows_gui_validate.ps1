param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$Backend,
    [string]$Image = "",
    [ValidateSet("FAT32", "NTFS", "exFAT", "ext4")][string]$FileSystem = "NTFS",
    [switch]$CreateImage,
    [switch]$DiscoverBackend,
    [ValidateSet("none", "mbr", "gpt")][string]$Table = "gpt",
    [string]$Directory = "artifacts/windows/gui-validation",
    [string]$Output = "artifacts/windows/gui-results.json"
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
# ValidateSet accepts case-insensitive input; the GUI's filesystem IDs are
# canonical strings and its integration assertions compare them exactly.
$FileSystem = switch ($FileSystem.ToLowerInvariant()) {
    "fat32" { "FAT32" }
    "ntfs" { "NTFS" }
    "exfat" { "exFAT" }
    "ext4" { "ext4" }
}
$Binary = (Resolve-Path $Binary).Path
$Backend = (Resolve-Path $Backend).Path
if ($CreateImage) {
    if ($Image -or $FileSystem -eq "NTFS") { throw "Creation requires FAT32, exFAT or ext4 and no -Image argument." }
} elseif (-not $Image) { throw "Supply -Image or -CreateImage." }
else { $Image = (Resolve-Path $Image).Path }
$Directory = [IO.Path]::GetFullPath($Directory)
if (Test-Path $Directory) { throw "Use a new validation directory: $Directory" }
[IO.Directory]::CreateDirectory($Directory) | Out-Null
if ($CreateImage) { $Image = Join-Path $Directory "gui-created.img" }
$payload = Join-Path $Directory "payload.txt"
[IO.File]::WriteAllText($payload, "xEtch native WinUI integration $([char]0x4e2d)$([char]0x6587)`n", $utf8)
$reportPath = Join-Path $Directory "gui-results.json"
$config = @{image=$Image;filesystem=$FileSystem;create_image=[bool]$CreateImage;table=$Table;payload=$payload;readback=(Join-Path $Directory "readback.txt");output=(Join-Path $Directory "gui-saved.img");report=$reportPath;screenshot=(Join-Path $Directory "preview.png")}
$configPath = Join-Path $Directory "test-config.json"
[IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json), $utf8)
if ($DiscoverBackend) {
    foreach ($name in @("XETCH_BACKEND", "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON", "XETCH_NTFS_WORKER", "QEMU_MODULE_DIR", "DOTNET_ROOT")) {
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $env:DOTNET_MULTILEVEL_LOOKUP = "0"
} else { $env:XETCH_BACKEND = $Backend }
$env:XETCH_WORKSPACE_ROOT = Join-Path $Directory "sessions"
$sourceHash = if (-not $CreateImage) { (Get-FileHash $Image).Hash } else { $null }
Invoke-Tool $Binary @("--smoke-test", $configPath) -WorkingDirectory $(if ($DiscoverBackend) { $Directory } else { "" }) | Out-Null
if (-not (Test-Path $reportPath)) { throw "GUI exited without a validation report. Use a build with -SmokeTests." }
$report = [IO.File]::ReadAllText($reportPath) | ConvertFrom-Json
if (-not $CreateImage -and (Get-FileHash $Image).Hash -ne $sourceHash) { throw "GUI test changed source bytes" }
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Output))) | Out-Null
[IO.File]::WriteAllText([IO.Path]::GetFullPath($Output), ($report | ConvertTo-Json -Depth 10), $utf8)
if ($report.status -ne "pass") { throw $report.error }
if (-not $report.source_unchanged) { throw "GUI source hash check did not pass" }
Write-Host "PASS native WinUI controller, rendering and backend integration"
