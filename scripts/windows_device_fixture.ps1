param(
    [Parameter(Mandatory=$true)][ValidateSet('Attach','Detach','DriverCheck')][string]$Action,
    [Parameter(Mandatory=$true)][string]$Root,
    [string]$ExpectedSha256 = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. "$PSScriptRoot/windows_common.ps1"
[Console]::OutputEncoding = $utf8
$rootPath = (Resolve-Path -LiteralPath $Root).Path
if ((Get-Item -LiteralPath $rootPath).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Fixture root cannot be linked.' }
$marker = [IO.File]::ReadAllText((Join-Path $rootPath '.xetch-device-validation.json'), $utf8) | ConvertFrom-Json
if ($marker.purpose -ne 'xetch-disposable-device-validation-v1' -or $marker.version -ne 1 -or $marker.image_name -ne 'fixture.vhd' -or $marker.size -ne 128MB -or $marker.partition_offset -ne 1MB -or $marker.partition_size -ne 127MB) { throw 'Invalid disposable fixture marker.' }
$image = Join-Path $rootPath 'fixture.vhd'
$attributes = (Get-Item -LiteralPath $image).Attributes
if ($attributes -band ([IO.FileAttributes]::ReparsePoint -bor [IO.FileAttributes]::Compressed -bor [IO.FileAttributes]::Encrypted -bor [IO.FileAttributes]::SparseFile)) { throw 'Fixture image must be a normal uncompressed fixed VHD.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Administrator rights are required only for this newly created disposable VHD.' }
$associated = Get-DiskImage -ImagePath $image
if ($Action -eq 'Attach') {
    if ($associated.Attached) { throw 'Fixture is already attached.' }
    $associated = Mount-DiskImage -ImagePath $image -Access ReadWrite -NoDriveLetter -PassThru
}
if (-not $associated.Attached) {
    if ($Action -eq 'Detach') { [Console]::Out.WriteLine((@{detached=$true} | ConvertTo-Json -Compress)); [Console]::Out.Flush(); exit 0 }
    throw 'Fixture is not attached.'
}
$disk = $associated | Get-Disk
if (@($disk).Count -ne 1 -or $disk.Size -ne $marker.size -or $disk.IsSystem -or $disk.IsBoot -or $associated.DevicePath -ne "\\.\PhysicalDrive$($disk.Number)") { throw 'Unexpected private VHD association; no physical target is accepted.' }
if ($Action -eq 'Detach') {
    Dismount-DiskImage -ImagePath $image | Out-Null
    if ((Get-DiskImage -ImagePath $image).Attached) { throw 'Private VHD is still attached.' }
    [Console]::Out.WriteLine((@{detached=$true} | ConvertTo-Json -Compress))
    [Console]::Out.Flush()
    exit 0
}
if ($disk.IsOffline) { $disk | Set-Disk -IsOffline $false | Out-Null }
$parts = @($disk | Get-Partition)
if ($parts.Count -ne 1 -or $parts[0].Offset -ne $marker.partition_offset -or $parts[0].Size -ne $marker.partition_size -or $parts[0].IsSystem -or $parts[0].IsBoot -or $parts[0].IsActive -or $parts[0].MbrType -ne 12) { throw 'Fixture partition table is not the expected non-system FAT32 data partition.' }
$volume = $parts[0] | Get-Volume
$volumePath = [string]$volume.Path
if ($volume.FileSystemType -ne 'FAT32' -or -not $volumePath.StartsWith('\\?\Volume{')) { throw 'Native FAT32 volume was not discovered.' }
if ($Action -eq 'DriverCheck') {
    if ($ExpectedSha256 -notmatch '^[0-9a-f]{64}$') { throw 'A full payload hash is required.' }
    $payload = Join-Path $volumePath 'planned.txt'
    if ((Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ExpectedSha256) { throw 'Native filesystem readback differs.' }
    $feedback = Join-Path $volumePath 'driver-feedback.txt'
    $stream = [IO.File]::Open($feedback,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
    try { $bytes=$utf8.GetBytes("native Windows FAT32 feedback`n");$stream.Write($bytes,0,$bytes.Length);$stream.Flush($true) } finally { $stream.Dispose() }
    if (-not $parts[0].DriveLetter) { $parts[0] | Add-PartitionAccessPath -AssignDriveLetter | Out-Null }
    $part = Get-Partition -DiskNumber $disk.Number -PartitionNumber $parts[0].PartitionNumber
    if (-not $part.DriveLetter) { throw 'Private VHD drive letter assignment failed.' }
    $result = Invoke-Tool (Join-Path $env:SystemRoot 'System32\chkdsk.exe') @("$($part.DriveLetter):") -IncludeStandardError -TimeoutSeconds 120
    [IO.File]::WriteAllText((Join-Path $rootPath 'chkdsk.log'),$result,$utf8)
}
[Console]::Out.WriteLine((@{attached=$true;device_path=[string]$associated.DevicePath;volume_path=$volumePath;logical_sector_size=[uint32]$disk.LogicalSectorSize;physical_sector_size=[uint32]$disk.PhysicalSectorSize;driver_check=($Action -eq 'DriverCheck')} | ConvertTo-Json -Compress))
[Console]::Out.Flush()
