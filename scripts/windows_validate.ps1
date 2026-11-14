param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$QemuDirectory,
    [string]$Output = "artifacts/windows/p0-results.json",
    [switch]$KeepImages
)
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$utf8 = [System.Text.UTF8Encoding]::new($false)
$Binary = (Resolve-Path $Binary).Path
$QemuDirectory = (Resolve-Path $QemuDirectory).Path
$env:XETCH_QEMU_IMG = Join-Path $QemuDirectory "qemu-img.exe"
$env:XETCH_QEMU_STORAGE_DAEMON = Join-Path $QemuDirectory "qemu-storage-daemon.exe"
$env:PATH = "$QemuDirectory;$env:PATH"

. (Join-Path $PSScriptRoot "windows_common.ps1")
function Invoke-Xetch([string[]]$ToolArguments) {
    $text = Invoke-Tool $Binary (@("--json") + $ToolArguments)
    return $text | ConvertFrom-Json
}
function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
$checks = [System.Collections.Generic.List[object]]::new()
function Passed([string]$Name) {
    $checks.Add(@{test=$Name;status="pass"})
    Write-Host "PASS $Name"
}
function Source-Hashes([string]$Directory) {
    return @(Get-ChildItem $Directory -File | Sort-Object Name | ForEach-Object {
        "$($_.Name):$((Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash)"
    }) -join "`n"
}
$scratch = Join-Path (Get-Location) ("artifacts/windows/p0-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $scratch -Force | Out-Null
$report = @{schema_version=1;host=[Environment]::OSVersion.VersionString;runtime="native Windows x86_64";results=$checks;status="running"}
try {
    $doctor = Invoke-Xetch @("doctor")
    Assert-True ($doctor.os -eq "windows") "CLI must be a native Windows binary"
    Assert-True (@($doctor.tools | Where-Object { -not $_.available }).Count -eq 0) "Missing native QEMU dependency"
    $report.tools = $doctor.tools
    $profile = Join-Path $QemuDirectory "xetch-dependency-profile.json"
    if (Test-Path $profile) { $report.dependency_profile = Get-Content -Raw -LiteralPath $profile | ConvertFrom-Json }
    Passed "native_windows_dependencies"
    $image = Join-Path $scratch "source.img"
    Invoke-Xetch @("create", "--output", $image, "--size", "64MiB", "--table", "none") | Out-Null
    foreach ($table in @("mbr", "gpt")) {
        $partitioned = Join-Path $scratch "$table.img"
        Invoke-Xetch @("create", "--output", $partitioned, "--size", "64MiB", "--table", $table) | Out-Null
        $info = Invoke-Xetch @("inspect", $partitioned)
        Assert-True ($info.layout.table -eq $table.ToUpperInvariant()) "Partition table mismatch"
        Assert-True ($info.layout.partitions[0].filesystem -eq "FAT32") "FAT32 partition not detected"
        Passed "native_windows_$table"
    }
    $payload = Join-Path $scratch "payload.txt"
    [IO.File]::WriteAllText($payload, "xEtch Unicode: $([char]0x4e2d)$([char]0x6587)`n", $utf8)
    foreach ($subformat in @("monolithicSparse", "monolithicFlat", "twoGbMaxExtentSparse", "twoGbMaxExtentFlat", "streamOptimized")) {
        $directory = Join-Path $scratch $subformat
        New-Item -ItemType Directory -Path $directory | Out-Null
        $vmdk = Join-Path $directory "disk.vmdk"
        Invoke-Tool $env:XETCH_QEMU_IMG @("convert", "-f", "raw", "-O", "vmdk", "-o", "subformat=$subformat", $image, $vmdk) | Out-Null
        $before = Source-Hashes $directory
        $info = Invoke-Xetch @("inspect", $vmdk)
        Assert-True ($info.image.format -eq "vmdk") "VMDK format mismatch"
        $workspace = Join-Path $scratch "edit-$subformat"
        Invoke-Xetch @("workspace", "create", $vmdk, "--workspace", $workspace) | Out-Null
        Invoke-Xetch @("file", "mkdir", "--workspace", $workspace, "--path", "/settings") | Out-Null
        Invoke-Xetch @("file", "put", "--workspace", $workspace, "--source", $payload, "--destination", "/settings/config.txt") | Out-Null
        $outputImage = Join-Path $scratch "edited-$subformat.vmdk"
        Invoke-Xetch @("apply", "--workspace", $workspace, "--output", $outputImage) | Out-Null
        $extracted = Join-Path $scratch "read-$subformat.txt"
        Invoke-Xetch @("file", "get", $outputImage, "--source", "/settings/config.txt", "--destination", $extracted) | Out-Null
        Assert-True ((Get-FileHash $extracted).Hash -eq (Get-FileHash $payload).Hash) "File payload differs"
        Assert-True ((Source-Hashes $directory) -eq $before) "Source image changed"
        Passed "native_windows_${subformat}_read_edit_flush_reopen_export"
    }
    $unicodeDirectory = Join-Path $scratch ("space " + [char]0x4e2d + [char]0x6587)
    New-Item -ItemType Directory -Path $unicodeDirectory | Out-Null
    $unicodeImage = Join-Path $unicodeDirectory "disk.img"
    Invoke-Xetch @("create", "--output", $unicodeImage, "--size", "64MiB", "--table", "none") | Out-Null
    $unicodeWorkspace = Join-Path $unicodeDirectory "edit session"
    Invoke-Xetch @("workspace", "create", $unicodeImage, "--workspace", $unicodeWorkspace) | Out-Null
    Invoke-Xetch @("file", "put", "--workspace", $unicodeWorkspace, "--source", $payload, "--destination", "/unicode.txt") | Out-Null
    Passed "native_windows_unicode_and_space_paths"
    $manifestBefore = [IO.File]::ReadAllText((Join-Path $unicodeWorkspace "workspace.json"))
    Invoke-Tool $Binary @("file", "put", "--workspace", $unicodeWorkspace, "--source", $payload, "--destination", "/unicode.txt") -ExpectFailure | Out-Null
    Assert-True ([IO.File]::ReadAllText((Join-Path $unicodeWorkspace "workspace.json")) -eq $manifestBefore) "Failed operation committed"
    Invoke-Tool $Binary @("apply", "--workspace", $unicodeWorkspace, "--output", $unicodeImage) -ExpectFailure | Out-Null
    Passed "native_windows_failed_edit_and_output_collision"
    $rpc = @{jsonrpc="2.0";id=1;method="file.list";params=@{workspace=$unicodeWorkspace}} | ConvertTo-Json -Depth 8 -Compress
    $reply = (Invoke-Tool $Binary @("serve", "--stdio") ($rpc + "`n")) | ConvertFrom-Json
    Assert-True ($null -ne $reply.result) ("RPC returned an error: " + ($reply | ConvertTo-Json -Depth 8 -Compress))
    Assert-True ($reply.result.entries[0].name -eq "unicode.txt") "RPC browse failed"
    Passed "native_windows_json_rpc"
    $qemuIo = Join-Path $QemuDirectory "qemu-io.exe"
    if (Test-Path $qemuIo) {
        $large = Join-Path $scratch "large-split.vmdk"
        Invoke-Tool $env:XETCH_QEMU_IMG @("create", "-f", "vmdk", "-o", "subformat=twoGbMaxExtentSparse", $large, "5G") | Out-Null
        $overlay = Join-Path $scratch "large.qcow2"
        Invoke-Tool $env:XETCH_QEMU_IMG @("create", "-f", "qcow2", "-F", "vmdk", "-b", $large, $overlay) | Out-Null
        foreach ($offset in @("2147483136", "2147483648", "4294971392")) {
            Invoke-Tool $qemuIo @("-f", "qcow2", "-c", "write -P 65 $offset 512", $overlay) | Out-Null
        }
        $saved = Join-Path $scratch "large-edited.vmdk"
        Invoke-Tool $env:XETCH_QEMU_IMG @("convert", "-f", "qcow2", "-O", "vmdk", $overlay, $saved) | Out-Null
        foreach ($offset in @("2147483136", "2147483648", "4294971392")) {
            $data = Invoke-Xetch @("sector", "read", $saved, "--offset", $offset, "--length", "512")
            Assert-True ($data.hex -eq ("41" * 512)) "64-bit NBD read mismatch"
        }
        Passed "native_windows_5GiB_split_extent_and_64bit_offsets"
    } else { throw "qemu-io.exe is required for the 64-bit offset check" }
    $report.status = "pass"
} catch {
    $report.status = "fail"
    $report.error = $_.ToString()
    Write-Host $report.error
} finally {
    $report.scratch = if ($KeepImages) { $scratch } else { $null }
    $outputPath = [IO.Path]::GetFullPath($Output)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputPath)) | Out-Null
    [IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 12), $utf8)
    if (-not $KeepImages) { Remove-Item -LiteralPath $scratch -Recurse -Force }
}
if ($report.status -ne "pass") { exit 1 }
