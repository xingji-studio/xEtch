# Embedded, read-only Storage/CIM queries. No device writes, mounts or elevation.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$warnings = [Collections.Generic.List[string]]::new()
$devices = [Collections.Generic.List[object]]::new()
$paths = $env:XETCH_DISCOVERY_PATHS | ConvertFrom-Json
$disks = @(Get-Disk -ErrorAction Stop)
$pageFiles = @()
try { $pageFiles = @(Get-CimInstance Win32_PageFileUsage -ErrorAction Stop | ForEach-Object { $_.Name }) }
catch { $warnings.Add('Page-file inventory is unavailable; protection cannot be relaxed.') }
$dumpPaths = @()
try {
    $control = Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\CrashControl'
    foreach ($key in @('DumpFile','MinidumpDir','DedicatedDumpFile')) {
        if ($control.$key) { $dumpPaths += [Environment]::ExpandEnvironmentVariables([string]$control.$key).Replace('\??\','') }
    }
} catch { $warnings.Add('Crash-dump configuration is unavailable; protection cannot be relaxed.') }
function Roles-AtPaths($accessPaths, $pairs) {
    $roles = @()
    foreach ($pair in $pairs) {
        foreach ($access in $accessPaths) {
            if ($access -and $pair.path -and ([string]$pair.path).StartsWith([string]$access,[StringComparison]::OrdinalIgnoreCase)) {
                $roles += $pair.role; break
            }
        }
    }
    return $roles
}
$pairs = @(@{role='runtime';path=$paths.runtime},@{role='workspace';path=$paths.workspace},@{role='runtime';path=$paths.temporary},@{role='hibernation';path=($env:SystemDrive + '\hiberfil.sys')})
foreach ($p in $pageFiles) { $pairs += @{role='pagefile';path=$p} }
foreach ($p in $dumpPaths) { $pairs += @{role='crash_dump';path=$p} }
foreach ($disk in $disks) {
    $diskId = "windows:disk:$($disk.Number)"
    $roles = @()
    if ($disk.IsSystem) { $roles += 'system' }
    if ($disk.IsBoot -or $disk.BootFromDisk) { $roles += 'boot' }
    if ($disk.IsClustered -or [string]$disk.BusType -in @('Spaces','RAID','Unknown','iSCSI','Fibre Channel')) { $roles += 'complex_storage' }
    $complete = $true
    try { $parts = @(Get-Partition -DiskNumber $disk.Number -ErrorAction Stop) }
    catch { $parts=@();$complete=$false;$warnings.Add("Partition inventory unavailable for $diskId.") }
    $devices.Add(@{id=$diskId;os_path="\\.\PhysicalDrive$($disk.Number)";name=[string]$disk.FriendlyName;kind='disk';size=[uint64]$disk.Size;offset=$null;logical_sector_size=[uint32]$disk.LogicalSectorSize;physical_sector_size=[uint32]$disk.PhysicalSectorSize;read_only=[bool]$disk.IsReadOnly;removable=([string]$disk.BusType -in @('USB','SD','MMC'));model=[string]$disk.Model;serial=[string]$disk.SerialNumber;stable_identity=[string]$disk.UniqueId;disk_number=[uint32]$disk.Number;partition_number=$null;partition_table=[string]$disk.PartitionStyle;partition_type=$null;partition_uuid=$null;filesystem=$null;label=$null;mount_points=@();parents=@();roles=@($roles);metadata_complete=$complete})
    foreach ($part in $parts) {
        $roles=@();$fs=$null;$label=$null;$complete=$true
        $access=@($part.AccessPaths | Where-Object { $_ })
        if ($part.IsBoot) { $roles += 'system' }
        if ($part.IsSystem) { $roles += 'boot' }
        if ($part.IsActive) { $roles += 'active_mbr' }
        if ($part.IsHidden -or $part.IsShadowCopy) { $roles += 'complex_storage' }
        $roles += @(Roles-AtPaths $access $pairs)
        try {
            $volumes=@($part | Get-Volume -ErrorAction Stop)
            if ($volumes.Count -eq 1) {
                $fs=[string]$volumes[0].FileSystemType;$label=[string]$volumes[0].FileSystemLabel
                if ([string]$volumes[0].HealthStatus -ne 'Healthy') { $complete=$false }
            } elseif ($volumes.Count -gt 1) { $roles += 'complex_storage';$complete=$false }
        } catch { $complete=$false }
        $type=if ([string]$disk.PartitionStyle -eq 'GPT') { [string]$part.GptType } else { '0x{0:x2}' -f [uint32]$part.MbrType }
        $stable=if ($part.Guid) { [string]$part.Guid } else { '' }
        $devices.Add(@{id="$diskId`:partition:$($part.PartitionNumber)";os_path="\\?\GLOBALROOT\Device\Harddisk$($disk.Number)\Partition$($part.PartitionNumber)";name="Partition $($part.PartitionNumber)";kind='partition';size=[uint64]$part.Size;offset=[uint64]$part.Offset;logical_sector_size=[uint32]$disk.LogicalSectorSize;physical_sector_size=[uint32]$disk.PhysicalSectorSize;read_only=([bool]$disk.IsReadOnly -or [bool]$part.IsReadOnly);removable=([string]$disk.BusType -in @('USB','SD','MMC'));model=[string]$disk.Model;serial=$null;stable_identity=$stable;disk_number=[uint32]$disk.Number;partition_number=[uint32]$part.PartitionNumber;partition_table=[string]$disk.PartitionStyle;partition_type=$type;partition_uuid=[string]$part.Guid;filesystem=$fs;label=$label;mount_points=@($access);parents=@($diskId);roles=@($roles | Select-Object -Unique);metadata_complete=$complete})
    }
}
# Get-Disk omits some dynamic/unsupported storage. Keep those devices visible and denied.
try {
    foreach ($legacy in @(Get-CimInstance Win32_DiskDrive -ErrorAction Stop)) {
        if ($disks.Number -contains [uint32]$legacy.Index) { continue }
        $devices.Add(@{id="windows:disk:$($legacy.Index)";os_path=[string]$legacy.DeviceID;name=[string]$legacy.Model;kind='disk';size=[uint64]$legacy.Size;offset=$null;logical_sector_size=[uint32]$legacy.BytesPerSector;physical_sector_size=0;read_only=$null;removable=$false;model=[string]$legacy.Model;serial=[string]$legacy.SerialNumber;stable_identity=$null;disk_number=[uint32]$legacy.Index;partition_number=$null;partition_table=$null;partition_type=$null;partition_uuid=$null;filesystem=$null;label=$null;mount_points=@();parents=@();roles=@('complex_storage');metadata_complete=$false})
        try {
            foreach ($legacyPart in @($legacy | Get-CimAssociatedInstance -ResultClassName Win32_DiskPartition -ErrorAction Stop)) {
                $access=@();$roles=@('complex_storage');$fs=$null;$label=$null
                if ($legacyPart.BootPartition -or $legacyPart.Bootable) { $roles += 'boot' }
                try {
                    foreach ($logical in @($legacyPart | Get-CimAssociatedInstance -ResultClassName Win32_LogicalDisk -ErrorAction Stop)) {
                        $access += ([string]$logical.DeviceID + '\');$fs=[string]$logical.FileSystem;$label=[string]$logical.VolumeName
                        if ([string]$logical.DeviceID -eq $env:SystemDrive) { $roles += 'system' }
                    }
                } catch { $warnings.Add('Legacy partition mount associations are unavailable.') }
                $roles += @(Roles-AtPaths $access $pairs)
                $devices.Add(@{id="windows:disk:$($legacy.Index):legacy-partition:$($legacyPart.Index)";os_path=[string]$legacyPart.DeviceID;name=[string]$legacyPart.Name;kind='partition';size=[uint64]$legacyPart.Size;offset=[uint64]$legacyPart.StartingOffset;logical_sector_size=[uint32]$legacy.BytesPerSector;physical_sector_size=0;read_only=$null;removable=$false;model=[string]$legacy.Model;serial=$null;stable_identity=$null;disk_number=[uint32]$legacy.Index;partition_number=$null;partition_table=$null;partition_type=[string]$legacyPart.Type;partition_uuid=$null;filesystem=$fs;label=$label;mount_points=@($access);parents=@("windows:disk:$($legacy.Index)");roles=@($roles | Select-Object -Unique);metadata_complete=$false})
            }
        } catch { $warnings.Add('Legacy partition discovery is unavailable; disk remains protected.') }
        $warnings.Add("Disk $($legacy.Index) is outside the supported basic Storage API topology.")
    }
} catch { $warnings.Add('Legacy disk cross-check is unavailable; inventory may be incomplete.') }
$warnings.Add('Filesystem encryption/clean state and exclusive access are not established by read-only discovery.')
@{devices=@($devices.ToArray());warnings=@($warnings.ToArray())} | ConvertTo-Json -Depth 12 -Compress
