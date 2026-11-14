param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$QemuDirectory,
    [Parameter(Mandatory=$true)][string]$Fixtures,
    [string]$ResultsDirectory = "artifacts/windows/filesystems",
    [string]$Output = "artifacts/windows/filesystems-results.json"
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$Binary = (Resolve-Path $Binary).Path
$QemuDirectory = (Resolve-Path $QemuDirectory).Path
$Fixtures = (Resolve-Path $Fixtures).Path
$env:XETCH_QEMU_IMG = Join-Path $QemuDirectory "qemu-img.exe"
$env:XETCH_QEMU_STORAGE_DAEMON = Join-Path $QemuDirectory "qemu-storage-daemon.exe"
$env:PATH = "$QemuDirectory;$env:PATH"
[IO.Directory]::CreateDirectory([IO.Path]::GetFullPath($ResultsDirectory)) | Out-Null
$ResultsDirectory = (Resolve-Path $ResultsDirectory).Path
function Xetch([string[]]$ToolArguments) { return (Invoke-Tool $Binary (@("--json") + $ToolArguments)) | ConvertFrom-Json }
function Check([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }
$checks = [System.Collections.Generic.List[object]]::new()
$report = @{schema_version=1;host=[Environment]::OSVersion.VersionString;runtime="native Windows x86_64";status="running";filesystems=$checks}
try {
    foreach ($fs in @("exfat", "ext4", "ntfs")) {
        $image = Join-Path $Fixtures "$fs.img"
        $sourceHash = (Get-FileHash $image).Hash
        Xetch @("file", "list", $image) | Out-Null
        if ($fs -ne "exfat") {
            $seed = Join-Path $ResultsDirectory "$fs-seed.txt"
            Xetch @("file", "get", $image, "--source", "/seed.txt", "--destination", $seed) | Out-Null
            Check ((Get-FileHash $seed).Hash -eq (Get-FileHash (Join-Path $Fixtures "seed.txt")).Hash) "Independent seed differs"
        }
        $workspace = Join-Path $ResultsDirectory "$fs-session"
        Xetch @("workspace", "create", $image, "--workspace", $workspace) | Out-Null
        $manifestPath = Join-Path $workspace "workspace.json"
        $before = [IO.File]::ReadAllText($manifestPath)
        [string[]]$optIn = @()
        if ($fs -ne "ntfs") { $optIn = @("--experimental-filesystems") }
        if ($fs -ne "ntfs") {
            Invoke-Tool $Binary @("file", "mkdir", "--workspace", $workspace, "--path", "/new-dir") -ExpectFailure | Out-Null
            Check ([IO.File]::ReadAllText($manifestPath) -eq $before) "Write opt-in guard changed workspace"
        } else {
            Check ((Xetch @("doctor")).capabilities.ntfs_write) "NTFS-3G worker is required for this validation"
        }
        Xetch ($optIn + @("file", "mkdir", "--workspace", $workspace, "--path", "/new-dir")) | Out-Null
        $payload = Join-Path $ResultsDirectory "$fs-payload.txt"
        [IO.File]::WriteAllText($payload, "Windows native $fs - $([char]0x4e2d)$([char]0x6587)`n", $utf8)
        Xetch ($optIn + @("file", "put", "--workspace", $workspace, "--source", $payload, "--destination", "/new-dir/data.txt")) | Out-Null
        [IO.File]::WriteAllText($payload, "replacement`n", $utf8)
        Xetch ($optIn + @("file", "put", "--workspace", $workspace, "--source", $payload, "--destination", "/new-dir/data.txt", "--replace")) | Out-Null
        Xetch ($optIn + @("file", "mkdir", "--workspace", $workspace, "--path", "/parent")) | Out-Null
        if ($fs -ne "ntfs") {
            Xetch ($optIn + @("file", "rename", "--workspace", $workspace, "--source", "/new-dir", "--destination", "/renamed-dir")) | Out-Null
            Xetch ($optIn + @("file", "rename", "--workspace", $workspace, "--source", "/renamed-dir", "--destination", "/parent/new-dir")) | Out-Null
            Xetch ($optIn + @("file", "rename", "--workspace", $workspace, "--source", "/parent/new-dir", "--destination", "/new-dir")) | Out-Null
        }
        Xetch ($optIn + @("file", "rename", "--workspace", $workspace, "--source", "/new-dir/data.txt", "--destination", "/parent/renamed.txt")) | Out-Null
        Xetch ($optIn + @("file", "rename", "--workspace", $workspace, "--source", "/parent/renamed.txt", "--destination", "/new-dir/data.txt")) | Out-Null
        Xetch ($optIn + @("file", "remove", "--workspace", $workspace, "--path", "/parent")) | Out-Null
        $raw = Join-Path $ResultsDirectory "$fs-saved.img"
        $vmdk = Join-Path $ResultsDirectory "$fs-saved.vmdk"
        Xetch @("apply", "--workspace", $workspace, "--output", $raw) | Out-Null
        Xetch @("apply", "--workspace", $workspace, "--output", $vmdk) | Out-Null
        $readback = Join-Path $ResultsDirectory "$fs-readback.txt"
        Xetch @("file", "get", $vmdk, "--source", "/new-dir/data.txt", "--destination", $readback) | Out-Null
        Check ((Get-FileHash $readback).Hash -eq (Get-FileHash $payload).Hash) "Saved content differs"
        Xetch ($optIn + @("file", "remove", "--workspace", $workspace, "--path", "/new-dir/data.txt")) | Out-Null
        Xetch ($optIn + @("file", "remove", "--workspace", $workspace, "--path", "/new-dir")) | Out-Null
        $operations = @("read", "mkdir_put_replace_remove", "file_rename_and_move", "raw_and_vmdk_export", "readback")
        if ($fs -ne "ntfs") { $operations += "nonempty_directory_rename_and_move" }
        $checks.Add(@{filesystem=$fs;status="pass";experimental_write=($fs -ne "ntfs");raw_output=$raw;checks=$operations})
        Check ((Get-FileHash $image).Hash -eq $sourceHash) "Source changed"
        Write-Host "PASS native Windows $fs"
    }
    $report.status = "pass"
} catch { $report.status = "fail"; $report.error = $_.ToString(); Write-Host $report.error }
$outputPath = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputPath)) | Out-Null
[IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 10), $utf8)
if ($report.status -ne "pass") { exit 1 }
