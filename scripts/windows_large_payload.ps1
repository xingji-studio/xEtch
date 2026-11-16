param([Parameter(Mandatory=$true)][string]$Root,[Parameter(Mandatory=$true)][string]$Token)
$ErrorActionPreference = "Stop"
# Only create a fixed regular sample in an existing, marked disposable directory.
$directory = [IO.Path]::GetFullPath($Root)
$marker = Get-Content -Raw -LiteralPath (Join-Path $directory ".xetch-fs-validation.json") | ConvertFrom-Json
if ($marker.purpose -ne "xetch-filesystem-validation-v1" -or $marker.token -ne $Token) { throw "Validation marker mismatch" }
$path = Join-Path $directory "large-payload.bin"
$size = [Int64]4297064585
$buffer = [byte[]]::new(1048576)
for ($i=0; $i -lt $buffer.Length; $i++) { $buffer[$i] = [byte]($i -band 255) }
$stream = [IO.FileStream]::new($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None,1048576,([IO.FileOptions]::SequentialScan -bor [IO.FileOptions]::WriteThrough))
try {
    for ([Int64]$offset=0; $offset -lt $size; $offset += $buffer.Length) {
        [BitConverter]::GetBytes([UInt64]$offset).CopyTo($buffer,0)
        $count = [int][Math]::Min([Int64]$buffer.Length,$size-$offset)
        $stream.Write($buffer,0,$count)
    }
    $stream.Flush($true)
} finally { $stream.Dispose() }
$inputStream = [IO.File]::OpenRead($path)
$algorithm = [Security.Cryptography.SHA256]::Create()
try { $hash = [BitConverter]::ToString($algorithm.ComputeHash($inputStream)).Replace("-","").ToLowerInvariant() }
finally { $algorithm.Dispose(); $inputStream.Dispose() }
@{size=$size;sha256=$hash;generator="native Windows .NET streaming"} | ConvertTo-Json -Compress
