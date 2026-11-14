param(
    [string]$Destination = "artifacts/windows/qemu",
    [string]$Installer = "artifacts/downloads/qemu-w64-setup-20260811.exe",
    [string]$SevenZip,
    [string]$ManifestTool,
    [switch]$AlreadyExtracted
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$expectedHash = "5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543"
$url = "https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe"
$destinationPath = [IO.Path]::GetFullPath($Destination)
# Never patch an arbitrary installed QEMU. The destination must be within this checkout.
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
if (-not $destinationPath.StartsWith($project + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Destination must be inside the project; installed QEMU is never modified"
}
$installerPath = [IO.Path]::GetFullPath($Installer)
$existingProfile = Join-Path $destinationPath "xetch-dependency-profile.json"
if (Test-Path $existingProfile) {
    $record = Get-Content -Raw -LiteralPath $existingProfile | ConvertFrom-Json
    $valid = $record.profile -eq "xetch-utf8-manifest-v1" -and $record.installer_sha512 -eq $expectedHash
    foreach ($entry in $record.binaries) {
        $file = Join-Path $destinationPath $entry.name
        $valid = $valid -and (Test-Path $file) -and ((Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant() -eq $entry.prepared_sha256)
    }
    if ($valid -and @($record.binaries).Count -eq 3) {
        Write-Output "Project-local QEMU UTF-8 profile already verified."
        return
    }
    throw "Existing QEMU profile differs from its record; choose a fresh project-local destination"
}
if (-not (Test-Path $installerPath)) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($installerPath)) | Out-Null
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $installerPath
}
if ((Get-FileHash -Algorithm SHA512 -LiteralPath $installerPath).Hash.ToLowerInvariant() -ne $expectedHash) {
    throw "QEMU installer SHA-512 mismatch"
}
if (-not $AlreadyExtracted) {
    if (Test-Path $destinationPath) { throw "Destination exists; choose a new project-local directory" }
    if (-not $SevenZip) {
        $SevenZip = Join-Path $env:ProgramFiles "7-Zip/7z.exe"
    }
    if (-not (Test-Path $SevenZip)) { throw "Specify -SevenZip pointing to 7z.exe" }
    Invoke-Tool $SevenZip @("x", $installerPath, "-o$destinationPath", "*.dll", "qemu-img.exe", "qemu-storage-daemon.exe", "qemu-io.exe", "COPYING*", "-r", "-y") | Out-Null
}
if (-not $ManifestTool) {
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits/10/bin"
    $ManifestTool = Get-ChildItem "$kits/*/x64/mt.exe" | Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $ManifestTool -or -not (Test-Path $ManifestTool)) { throw "Windows SDK mt.exe is required for the UTF-8 dependency profile" }
$provenance = @{url=$url;installer_sha512=$expectedHash;profile="xetch-utf8-manifest-v1";binaries=@()}
foreach ($name in @("qemu-img.exe", "qemu-storage-daemon.exe", "qemu-io.exe")) {
    $binary = Join-Path $destinationPath $name
    $originalHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLowerInvariant()
    $manifestPath = "$binary.manifest.xml"
    try {
        Invoke-Tool $ManifestTool @("-nologo", "-inputresource:$binary;#1", "-out:$manifestPath") | Out-Null
    } catch {
        if ($_.ToString() -notmatch "c101008c") { throw }
        # The pinned QEMU tools contain VERSIONINFO but no manifest resource.
        [IO.File]::WriteAllText($manifestPath, '<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"/>', $utf8)
    }
    [xml]$document = [IO.File]::ReadAllText($manifestPath)
    $ns = [Xml.XmlNamespaceManager]::new($document.NameTable)
    $ns.AddNamespace("a", "urn:schemas-microsoft-com:asm.v1")
    $ns.AddNamespace("v3", "urn:schemas-microsoft-com:asm.v3")
    $ns.AddNamespace("s19", "http://schemas.microsoft.com/SMI/2019/WindowsSettings")
    $application = $document.SelectSingleNode("/a:assembly/v3:application", $ns)
    if (-not $application) {
        $application = $document.CreateElement("application", "urn:schemas-microsoft-com:asm.v3")
        $document.DocumentElement.AppendChild($application) | Out-Null
    }
    $settings = $application.SelectSingleNode("v3:windowsSettings", $ns)
    if (-not $settings) {
        $settings = $document.CreateElement("windowsSettings", "urn:schemas-microsoft-com:asm.v3")
        $application.AppendChild($settings) | Out-Null
    }
    $codepage = $settings.SelectSingleNode("s19:activeCodePage", $ns)
    if (-not $codepage) {
        $codepage = $document.CreateElement("activeCodePage", "http://schemas.microsoft.com/SMI/2019/WindowsSettings")
        $settings.AppendChild($codepage) | Out-Null
    }
    $codepage.InnerText = "UTF-8"
    $document.Save($manifestPath)
    Invoke-Tool $ManifestTool @("-nologo", "-manifest", $manifestPath, "-outputresource:$binary;#1") | Out-Null
    $provenance.binaries += @{name=$name;original_sha256=$originalHash;prepared_sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLowerInvariant()}
}
[IO.File]::WriteAllText((Join-Path $destinationPath "xetch-dependency-profile.json"), ($provenance | ConvertTo-Json -Depth 8), $utf8)
Write-Output "Prepared project-local QEMU UTF-8 profile; no system locale or installation changed."
