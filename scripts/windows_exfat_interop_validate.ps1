param(
    [Parameter(Mandatory=$true)][string]$Directory,
    [Parameter(Mandatory=$true)][string]$Output,
    [string]$WindowsBackend = "target/windows/release/xetch.exe",
    [string]$LinuxBackend = "",
    [string]$LargePayload = "",
    [switch]$SkipLargeFile,
    [string]$Distribution = "Ubuntu-22.04",
    [string]$QemuDirectory = "artifacts/windows/qemu"
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
Set-Location -LiteralPath (Split-Path $PSScriptRoot -Parent)
trap {
    $failure = if ($report -is [Collections.IDictionary]) { $report } else { @{schema_version=1;driver="Microsoft Windows exFAT";cases=@()} }
    $failure.status = "fail"
    $failure.error = $_.ToString()
    $failure.error_position = $_.InvocationInfo.PositionMessage
    $failure.script_stack_trace = $_.ScriptStackTrace
    if ($Output) {
        $failurePath = [IO.Path]::GetFullPath($Output)
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($failurePath)) | Out-Null
        [IO.File]::WriteAllText($failurePath, ($failure | ConvertTo-Json -Depth 8), $utf8)
    }
    exit 1
}
$root = [IO.Path]::GetFullPath($Directory)
if (Test-Path -LiteralPath $root) { throw "Use a new disposable validation directory." }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Administrator rights are needed only to attach and format disposable VHD files."
}
[IO.Directory]::CreateDirectory($root) | Out-Null
$token = [guid]::NewGuid().ToString()
[IO.File]::WriteAllText((Join-Path $root ".xetch-fs-validation.json"), (@{purpose="xetch-filesystem-validation-v1";token=$token} | ConvertTo-Json), $utf8)
$runtime = Join-Path $root "runtime"
[IO.Directory]::CreateDirectory($runtime) | Out-Null
Copy-Item -LiteralPath (Resolve-Path -LiteralPath $WindowsBackend).Path -Destination (Join-Path $runtime "xetch.exe")
$WindowsBackend = Join-Path $runtime "xetch.exe"
$qemu = Join-Path (Resolve-Path -LiteralPath $QemuDirectory).Path "qemu-img.exe"
$env:XETCH_QEMU_IMG = $qemu
$env:XETCH_QEMU_STORAGE_DAEMON = Join-Path (Split-Path $qemu) "qemu-storage-daemon.exe"
$report = @{schema_version=1;status="running";driver="Microsoft Windows exFAT";cases=@();large_file_requested=(-not $SkipLargeFile);started_at=[DateTime]::UtcNow.ToString("o");windows_version=[Environment]::OSVersion.VersionString;windows_backend_sha256=(Get-FileHash -LiteralPath $WindowsBackend).Hash.ToLowerInvariant()}
if ($LinuxBackend) {
    $nativeLinux = (Invoke-Tool "$env:SystemRoot/System32/wsl.exe" @("-d", $Distribution, "--exec", "wslpath", "-w", $LinuxBackend) -MinimalQuoting).Trim()
    $linuxCopy = Join-Path $runtime "xetch-linux"
    Copy-Item -LiteralPath $nativeLinux -Destination $linuxCopy
    $LinuxBackend = (Invoke-Tool "$env:SystemRoot/System32/wsl.exe" @("-d", $Distribution, "--exec", "wslpath", "-u", $linuxCopy) -MinimalQuoting).Trim()
    Invoke-Tool "$env:SystemRoot/System32/wsl.exe" @("-d", $Distribution, "--exec", "chmod", "u+x", $LinuxBackend) -MinimalQuoting | Out-Null
    $report.linux_backend_sha256 = (Get-FileHash -LiteralPath $linuxCopy).Hash.ToLowerInvariant()
}
$outputPath = [IO.Path]::GetFullPath($Output)
function Save-Report {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputPath)) | Out-Null
    [IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 12), $utf8)
}
function Backend([string[]]$Arguments) {
    $arguments = @("--json", "--experimental-filesystems") + $Arguments
    if ($script:backendKind -eq "windows") { $text = Invoke-Tool $WindowsBackend $arguments -TimeoutSeconds 3600 }
    else {
        $converted = foreach ($argument in $arguments) {
            if ($argument.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
                (Invoke-Tool "$env:SystemRoot/System32/wsl.exe" @("-d", $Distribution, "--exec", "wslpath", "-u", $argument) -MinimalQuoting).Trim()
            } else { $argument }
        }
        $text = Invoke-Tool "$env:SystemRoot/System32/wsl.exe" (@("-d",$Distribution,"--exec","env","-u","XETCH_QEMU_IMG","-u","XETCH_QEMU_STORAGE_DAEMON",$LinuxBackend) + $converted) -TimeoutSeconds 3600 -MinimalQuoting
    }
    return $text | ConvertFrom-Json
}
function Native-Driver([string]$Image, [bool]$Format, $Expected, $Writes, $HostWrites = @{}) {
    $request = Join-Path $root ("request-"+[guid]::NewGuid().ToString("N")+".json")
    $result = "$request.result.json"
    $data = @{root=$root;token=$token;image=$Image;format=$Format;expect=$Expected;write=$Writes;write_from_host=$HostWrites;result=$result}
    [IO.File]::WriteAllText($request, ($data | ConvertTo-Json -Depth 10), $utf8)
    & (Join-Path $PSScriptRoot "windows_native_fs_roundtrip.ps1") -Request $request
    $answer = [IO.File]::ReadAllText($result, $utf8) | ConvertFrom-Json
    if ($answer.status -ne "pass") { throw "Native driver request failed." }
}
function Hex([byte[]]$Bytes) { [BitConverter]::ToString($Bytes).Replace("-", "").ToLowerInvariant() }
function Expected([string]$File) { @{size=(Get-Item -LiteralPath $File).Length;sha256=(Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()} }
try {
    $kinds = @("windows")
    if ($LinuxBackend) { $kinds += "linux" }
    foreach ($script:backendKind in $kinds) {
        $folder = Join-Path $root $script:backendKind
        [IO.Directory]::CreateDirectory($folder) | Out-Null
        $sourceVhd = Join-Path $folder "windows-created.vhd"
        Invoke-Tool $qemu @("create","-f","vpc","-o","subformat=fixed,force_size=on",$sourceVhd,"256M") | Out-Null
        $initial = $utf8.GetBytes(("native Windows exFAT seed`n" * 2048))
        Native-Driver $sourceVhd $true @{} @{"system-data.bin"=(Hex $initial);"directory/inside.txt"=(Hex ($utf8.GetBytes("preserved directory content")))}
        $source = Join-Path $folder "system-source.img"
        Invoke-Tool $qemu @("convert","-f","vpc","-O","raw",$sourceVhd,$source) | Out-Null
        $original = (Get-FileHash -LiteralPath $source).Hash
        $readback = Join-Path $folder "initial.bin"
        Backend @("file","get",$source,"--source","/system-data.bin","--destination",$readback) | Out-Null
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($readback)) -ne [Convert]::ToBase64String($initial)) { throw "Backend could not read Windows-created file." }
        $workspace = Join-Path $folder "session"
        Backend @("workspace","create",$source,"--workspace",$workspace) | Out-Null
        $payload = Join-Path $folder "payload.bin"
        [IO.File]::WriteAllText($payload, ("xEtch output for actual Windows exFAT driver`n" * 4096), $utf8)
        Backend @("file","put","--workspace",$workspace,"--source",$payload,"--destination","/system-data.bin","--replace") | Out-Null
        Backend @("file","rename","--workspace",$workspace,"--source","/directory","--destination","/renamed-directory") | Out-Null
        $saved = Join-Path $folder "saved.img"
        Backend @("apply","--workspace",$workspace,"--output",$saved) | Out-Null
        $savedVhd = Join-Path $folder "edited.vhd"
        Invoke-Tool $qemu @("convert","-f","raw","-O","vpc","-o","subformat=fixed,force_size=on",$saved,$savedVhd) | Out-Null
        $inside = Join-Path $folder "inside.txt"; [IO.File]::WriteAllText($inside,"preserved directory content",$utf8)
        $feedback = $utf8.GetBytes("Windows driver rewrote the backend export`n")
        Native-Driver $savedVhd $false @{"system-data.bin"=(Expected $payload);"renamed-directory/inside.txt"=(Expected $inside)} @{"driver-feedback.txt"=(Hex $feedback)}
        $returned = Join-Path $folder "returned.img"
        Invoke-Tool $qemu @("convert","-f","vpc","-O","raw",$savedVhd,$returned) | Out-Null
        $reply = Join-Path $folder "reply.txt"
        Backend @("file","get",$returned,"--source","/driver-feedback.txt","--destination",$reply) | Out-Null
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($reply)) -ne [Convert]::ToBase64String($feedback)) { throw "Returned driver content differs." }
        $fresh = Join-Path $folder "backend-created.img"
        Backend @("create","--output",$fresh,"--size","128MiB","--table","mbr","--filesystem","exfat") | Out-Null
        $freshVhd = Join-Path $folder "backend-created.vhd"
        Invoke-Tool $qemu @("convert","-f","raw","-O","vpc","-o","subformat=fixed,force_size=on",$fresh,$freshVhd) | Out-Null
        Native-Driver $freshVhd $false @{} @{"driver-on-backend.txt"=(Hex $feedback)}
        $freshReturned = Join-Path $folder "backend-created-returned.img"
        Invoke-Tool $qemu @("convert","-f","vpc","-O","raw",$freshVhd,$freshReturned) | Out-Null
        $freshRead = Join-Path $folder "backend-created-read.txt"
        Backend @("file","get",$freshReturned,"--source","/driver-on-backend.txt","--destination",$freshRead) | Out-Null
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($freshRead)) -ne [Convert]::ToBase64String($feedback)) { throw "Windows write to backend-formatted volume did not survive." }
        if ((Get-FileHash -LiteralPath $source).Hash -ne $original) { throw "Backend changed its original source." }
        $report.cases += @{backend=$script:backendKind;suite="windows-driver-roundtrip";status="pass";checks=@("windows_format_and_driver_write","backend_read_and_edit","windows_driver_full_hash_and_write","backend_reads_returned_file","windows_mounts_backend_formatted_exfat","read_only_chkdsk_after_each_mount","original_source_unchanged")}
        Save-Report
    }
    if (-not $SkipLargeFile) {
        if ([IO.DriveInfo]::new([IO.Path]::GetPathRoot($root)).AvailableFreeSpace -lt 27GB) { throw "Native large-file validation needs at least 27 GiB free." }
        $generated = ""
        if (-not $LargePayload) {
            $generated = Join-Path $root "generated-large-payload.bin"
            $block = [byte[]]::new(1MB)
            for ($i=0; $i -lt 256; $i++) { $block[$i] = $i }
            for ($length=256; $length -lt $block.Length; $length*=2) { [Array]::Copy($block,0,$block,$length,$length) }
            $size = 4GB + 2MB + 137
            $file = [IO.File]::Open($generated, [IO.FileMode]::CreateNew)
            try {
                for ([long]$offset=0; $offset -lt $size; $offset+=$block.Length) {
                    [BitConverter]::GetBytes($offset).CopyTo($block,0)
                    $file.Write($block,0,[int][Math]::Min($block.Length,$size-$offset))
                }
                $file.Flush($true)
            } finally { $file.Dispose() }
            $LargePayload = $generated
        }
        $script:backendKind = "windows"
        $folder = Join-Path $root "windows-large"
        [IO.Directory]::CreateDirectory($folder) | Out-Null
        $inputInfo = Get-Item -LiteralPath $LargePayload
        if ($inputInfo.Length -le 4GB -or $inputInfo.Length -gt 5GB) { throw "Large payload must be above 4 GiB and at most 5 GiB." }
        $payload = Join-Path $folder "large-input.bin"
        Copy-Item -LiteralPath $inputInfo.FullName -Destination $payload
        $expected = Expected $payload
        $sourceVhd = Join-Path $folder "windows-large.vhd"
        Invoke-Tool $qemu @("create","-f","vpc","-o","subformat=fixed,force_size=on",$sourceVhd,"6G") | Out-Null
        Native-Driver $sourceVhd $true @{} @{} @{"large.bin"=$payload}
        $source = Join-Path $folder "source.img"
        Invoke-Tool $qemu @("convert","-f","vpc","-O","raw",$sourceVhd,$source) | Out-Null
        Remove-Item -LiteralPath $sourceVhd
        $original = (Get-FileHash -LiteralPath $source).Hash
        $readback = Join-Path $folder "readback.bin"
        Backend @("file","get",$source,"--source","/large.bin","--destination",$readback) | Out-Null
        if ((Expected $readback).sha256 -ne $expected.sha256 -or (Get-Item -LiteralPath $readback).Length -ne $expected.size) { throw "Backend readback of Windows-created large file differs." }
        Remove-Item -LiteralPath $readback
        $workspace = Join-Path $folder "session"
        Backend @("workspace","create",$source,"--workspace",$workspace) | Out-Null
        Backend @("file","put","--workspace",$workspace,"--source",$payload,"--destination","/large.bin","--replace") | Out-Null
        Backend @("file","rename","--workspace",$workspace,"--source","/large.bin","--destination","/moved.bin") | Out-Null
        $saved = Join-Path $folder "saved.img"
        Backend @("apply","--workspace",$workspace,"--output",$saved) | Out-Null
        $savedVhd = Join-Path $folder "edited.vhd"
        Invoke-Tool $qemu @("convert","-f","raw","-O","vpc","-o","subformat=fixed,force_size=on",$saved,$savedVhd) | Out-Null
        Remove-Item -LiteralPath $saved
        $feedback = $utf8.GetBytes("Windows read the complete file above 4 GiB`n")
        Native-Driver $savedVhd $false @{"moved.bin"=$expected} @{"large-feedback.txt"=(Hex $feedback)}
        $returned = Join-Path $folder "returned.img"
        Invoke-Tool $qemu @("convert","-f","vpc","-O","raw",$savedVhd,$returned) | Out-Null
        Remove-Item -LiteralPath $savedVhd
        Backend @("file","get",$returned,"--source","/moved.bin","--destination",$readback) | Out-Null
        if ((Expected $readback).sha256 -ne $expected.sha256 -or (Get-Item -LiteralPath $readback).Length -ne $expected.size) { throw "Large file changed after Windows driver roundtrip." }
        Remove-Item -LiteralPath $readback
        Backend @("file","get",$returned,"--source","/large-feedback.txt","--destination",$readback) | Out-Null
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($readback)) -ne [Convert]::ToBase64String($feedback)) { throw "Native large-file feedback differs." }
        if ((Get-FileHash -LiteralPath $source).Hash -ne $original) { throw "Large-file source changed." }
        $report.cases += @{backend="windows";suite="large-file";status="pass";file_size=$expected.size;sha256=$expected.sha256;checks=@("windows_formats_and_streams_full_large_file","backend_full_hash_of_windows_created_file","backend_overwrites_and_renames_large_file","windows_driver_full_sha256_and_feedback_write","backend_full_sha256_after_windows_roundtrip","read_only_chkdsk_after_each_mount","original_source_unchanged")}
        Save-Report
        Remove-Item -LiteralPath $folder -Recurse
        if ($generated) { Remove-Item -LiteralPath $generated }
    }
    $report.status = "pass"
} catch { $report.status = "fail"; $report.error = $_.ToString(); throw }
finally { $report.finished_at = [DateTime]::UtcNow.ToString("o"); Save-Report }
