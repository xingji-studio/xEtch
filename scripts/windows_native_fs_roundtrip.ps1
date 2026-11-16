param([Parameter(Mandatory=$true)][string]$Request)
$ErrorActionPreference = "Stop"
$utf8 = [Text.UTF8Encoding]::new($false)
$requestData = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $Request).Path, $utf8) | ConvertFrom-Json
$root = [IO.Path]::GetFullPath($requestData.root).TrimEnd('\')
$marker = [IO.File]::ReadAllText((Join-Path $root ".xetch-fs-validation.json"), $utf8) | ConvertFrom-Json
if ($marker.purpose -ne "xetch-filesystem-validation-v1" -or $marker.token -ne $requestData.token) {
    throw "Missing or mismatched disposable validation directory marker."
}
$image = (Resolve-Path -LiteralPath $requestData.image).Path
if (-not $image.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase) -or
    [IO.Path]::GetExtension($image) -ne ".vhd" -or
    ((Get-Item -LiteralPath $image).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Only a regular VHD inside the marked validation directory is allowed."
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Native Windows exFAT driver validation requires administrator rights to attach this temporary VHD."
}
function Guest-Path([string]$Name) {
    if ([IO.Path]::IsPathRooted($Name) -or @($Name -split '[\\/]' | Where-Object { $_ -in @("", ".", "..") }).Count) {
        throw "Expected a relative guest path."
    }
    $path = [IO.Path]::GetFullPath((Join-Path $script:volumeRoot $Name))
    if (-not $path.StartsWith($script:volumeRoot, [StringComparison]::OrdinalIgnoreCase)) { throw "Guest path escapes VHD." }
    return $path
}
$attached = $false
$checks = [Collections.Generic.List[string]]::new()
try {
    if ((Get-DiskImage -ImagePath $image).Attached) { throw "The private test VHD is already attached." }
    # QEMU marks even fixed VHD output sparse on NTFS. The Windows VHD
    # driver refuses that attribute; clear it on this private image only.
    if ((Get-Item -LiteralPath $image).Attributes -band [IO.FileAttributes]::SparseFile) {
        $message = & "$env:SystemRoot/System32/fsutil.exe" sparse setflag $image 0 2>&1
        if ($LASTEXITCODE -ne 0) { throw "Cannot materialize private VHD: $message" }
        $checks.Add("private_vhd_sparse_attribute_cleared")
    }
    $unsupported = [IO.FileAttributes]::SparseFile -bor [IO.FileAttributes]::Compressed -bor [IO.FileAttributes]::Encrypted
    if ((Get-Item -LiteralPath $image).Attributes -band $unsupported) {
        throw "Windows VHD attachment requires an uncompressed, unencrypted, non-sparse test file."
    }
    $diskImage = Mount-DiskImage -ImagePath $image -Access ReadWrite -PassThru
    $attached = $true
    $disk = $diskImage | Get-Disk
    # Disk number comes only from this exact image association. Never search
    # for a free physical disk or accept a caller-provided disk number.
    if (@($disk).Count -ne 1 -or $disk.Size -lt 128MB -or $disk.Size -gt 8GB -or
        $diskImage.DevicePath -ne "\\.\PhysicalDrive$($disk.Number)") {
        throw "Unexpected temporary VHD association or size."
    }
    if ($disk.IsOffline) { Set-Disk -Number $disk.Number -IsOffline $false }
    if ($disk.IsReadOnly) { Set-Disk -Number $disk.Number -IsReadOnly $false }
    if ($requestData.format) {
        if ($disk.PartitionStyle -ne "RAW") { throw "Seed formatting requires a new, unpartitioned test VHD." }
        $disk | Initialize-Disk -PartitionStyle MBR -PassThru | Out-Null
        $partition = New-Partition -DiskNumber $disk.Number -UseMaximumSize -AssignDriveLetter
        $partition | Format-Volume -FileSystem exFAT -NewFileSystemLabel ("XETCH"+$requestData.token.Substring(0,8)) -Confirm:$false -Force | Out-Null
    } else {
        $partitions = @($disk | Get-Partition | Where-Object { $_.Type -ne "Reserved" -and $_.Size -gt 64MB })
        if ($partitions.Count -ne 1) { throw "Expected one data partition in the temporary VHD." }
        $partition = $partitions[0]
        if (-not $partition.DriveLetter) {
            $partition | Add-PartitionAccessPath -AssignDriveLetter
            $partition = Get-Partition -DiskNumber $disk.Number -PartitionNumber $partition.PartitionNumber
        }
    }
    $volume = $partition | Get-Volume
    if ($volume.FileSystem -ne "exFAT" -or -not $partition.DriveLetter) { throw "Windows did not recognize the exFAT filesystem." }
    $script:volumeRoot = "$($partition.DriveLetter):\"
    if ($requestData.expect) {
        foreach ($entry in $requestData.expect.PSObject.Properties) {
            $path = Guest-Path $entry.Name
            if ((Get-Item -LiteralPath $path).Length -ne $entry.Value.size -or
                (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.Value.sha256) {
                throw "Windows driver readback differs: $($entry.Name)"
            }
            $checks.Add("windows_driver_read:$($entry.Name)")
        }
    }
    if ($requestData.write) {
        foreach ($entry in $requestData.write.PSObject.Properties) {
            $path = Guest-Path $entry.Name
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
            $hex = $entry.Value
            $bytes = [byte[]]::new($hex.Length / 2)
            for ($i=0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [Convert]::ToByte($hex.Substring($i*2,2),16) }
            $file = [IO.File]::Open($path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $file.Write($bytes,0,$bytes.Length); $file.Flush($true) } finally { $file.Dispose() }
            $checks.Add("windows_driver_write:$($entry.Name)")
        }
    }
    if ($requestData.write_from_host) {
        foreach ($entry in $requestData.write_from_host.PSObject.Properties) {
            $source = (Resolve-Path -LiteralPath $entry.Value).Path
            if (-not $source.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase) -or
                ((Get-Item -LiteralPath $source).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw "Large-file input must be a regular file in the marked validation directory."
            }
            $path = Guest-Path $entry.Name
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
            $inputFile = [IO.File]::OpenRead($source)
            try {
                $outputFile = [IO.File]::Open($path, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try { $inputFile.CopyTo($outputFile, 1MB); $outputFile.Flush($true) } finally { $outputFile.Dispose() }
            } finally { $inputFile.Dispose() }
            $checks.Add("windows_driver_streamed_write:$($entry.Name)")
        }
    }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $env:SystemRoot "System32/chkdsk.exe"
    $start.Arguments = "$($partition.DriveLetter):"  # Read-only inspection; never /f or /r.
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $check = [Diagnostics.Process]::Start($start)
    $stdout = $check.StandardOutput.ReadToEndAsync(); $stderr = $check.StandardError.ReadToEndAsync()
    try {
        if (-not $check.WaitForExit(120000)) { $check.Kill(); throw "Read-only chkdsk timed out." }
        [IO.File]::WriteAllText(($Request + ".chkdsk.log"), ($stdout.Result + $stderr.Result), $utf8)
        if ($check.ExitCode -ne 0) { throw "Read-only chkdsk reported exFAT errors ($($check.ExitCode)); see $Request.chkdsk.log" }
    } finally { $check.Dispose() }
    $checks.Add("native_chkdsk_read_only")
    $report = @{status="pass";driver="Microsoft Windows exFAT";checks=$checks;disk_image=$image}
} finally {
    if ($attached) { Dismount-DiskImage -ImagePath $image }
}
$resultPath = [IO.Path]::GetFullPath($requestData.result)
if (-not $resultPath.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) { throw "Result must remain inside the marked directory." }
[IO.File]::WriteAllText($resultPath, ($report | ConvertTo-Json -Depth 8), $utf8)
Write-Host "PASS native Windows exFAT mount/read/write/dismount"
