param(
    [Parameter(Mandatory=$true)][string]$Directory,
    [Parameter(Mandatory=$true)][string]$Output,
    [ValidateSet('smoke','all')][string]$Suite = 'all',
    [string]$Binary = 'target/windows/release/xetch.exe',
    [string]$Probe = 'target/windows/release/examples/device_validation_probe.exe',
    [string]$Gui = '',
    [switch]$InventoryOnly,
    [switch]$Keep,
    [switch]$WaitForRetry,
    [ValidateRange(1,60)][int]$RetryMinutes = 20
)
$ErrorActionPreference = 'Stop'
trap {
    $launchError = @{status='fail';error=$_.ToString();position=$_.InvocationInfo.PositionMessage;script_stack_trace=$_.ScriptStackTrace}
    $launchErrorPath = [IO.Path]::GetFullPath($Output + '.launch-error.json')
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($launchErrorPath)) | Out-Null
    [IO.File]::WriteAllText($launchErrorPath,($launchError | ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
    try { Stop-Transcript | Out-Null } catch { }
    exit 1
}
. "$PSScriptRoot/windows_common.ps1"
$repositoryRoot = Split-Path $PSScriptRoot -Parent
Set-Location -LiteralPath $repositoryRoot
# Set-Location alone does not change the .NET/native working directory. RunAs
# starts in System32, so all native path resolution must be anchored explicitly.
[Environment]::CurrentDirectory = $repositoryRoot
$Directory = [IO.Path]::GetFullPath($Directory)
$Output = [IO.Path]::GetFullPath($Output)
$Binary = [IO.Path]::GetFullPath($Binary)
$Probe = [IO.Path]::GetFullPath($Probe)
if ($Gui) { $Gui = [IO.Path]::GetFullPath($Gui) }
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Output)) | Out-Null
Start-Transcript -LiteralPath ($Output + '.startup.log') -Force | Out-Null
$python = (Get-Command python.exe -ErrorAction Stop).Source
$deadline = [DateTime]::UtcNow.AddMinutes($RetryMinutes)
for ($attempt = 1; $attempt -le 5; $attempt++) {
    $testRoot = if ($attempt -eq 1) { $Directory } else { $Directory + '-retry-' + $attempt }
    $validationArguments = @("$PSScriptRoot/devices_validate.py",'--binary',$Binary,'--probe',$Probe,'--directory',$testRoot,'--output',$Output,'--suite',$Suite)
    if ($InventoryOnly) { $validationArguments += '--inventory-only' }
    if ($Keep) { $validationArguments += '--keep' }
    if ($Gui) { $validationArguments += @('--gui',$Gui) }
    try {
        Write-Host (Invoke-Tool $python $validationArguments -IncludeStandardError -TimeoutSeconds 1200)
        Stop-Transcript | Out-Null
        exit 0
    } catch {
        if (-not $WaitForRetry -or $attempt -eq 5) { throw }
        if (Test-Path -LiteralPath $Output) { Copy-Item -LiteralPath $Output -Destination ($Output + '.attempt-' + $attempt + '.json') }
        [IO.File]::WriteAllText(($Output + '.retry-ready.json'),(@{status='waiting';attempt=$attempt;error=$_.ToString();deadline=$deadline.ToString('o')} | ConvertTo-Json -Depth 8),$utf8)
        Write-Host 'Disposable test failed. Waiting for a bounded retry or stop request; no real-device commands are accepted.'
        while (-not (Test-Path -LiteralPath ($Output + '.retry'))) {
            if (Test-Path -LiteralPath ($Output + '.stop')) { throw 'Test retry session stopped.' }
            if ([DateTime]::UtcNow -ge $deadline) { throw 'Finite disposable-test retry session expired.' }
            Start-Sleep -Milliseconds 200
        }
        Remove-Item -LiteralPath ($Output + '.retry')
        Remove-Item -LiteralPath ($Output + '.retry-ready.json')
    }
}
