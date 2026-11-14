param(
    [Parameter(Mandatory=$true)][string]$Backend,
    [Parameter(Mandatory=$true)][string]$Gui,
    [Parameter(Mandatory=$true)][string]$Directory,
    [Parameter(Mandatory=$true)][string]$Output
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows_common.ps1")
$Backend = (Resolve-Path -LiteralPath $Backend).Path
$Gui = (Resolve-Path -LiteralPath $Gui).Path
if (Test-Path -LiteralPath $Directory) { throw "Use a new launcher validation directory." }
$Directory = [IO.Path]::GetFullPath($Directory)
[IO.Directory]::CreateDirectory($Directory) | Out-Null
foreach ($name in @("XETCH_GUI", "XETCH_BACKEND", "DOTNET_ROOT")) {
    [Environment]::SetEnvironmentVariable($name, $null, "Process")
}
$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
$env:DOTNET_MULTILEVEL_LOOKUP = "0"
$checks = [Collections.Generic.List[string]]::new()
$report = @{schema_version=1;status="running";platform="native Windows x64";checks=$checks}
function Own-Gui {
    Get-Process -Name "xEtch.Windows" -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Gui }
}
function Close-OwnGui {
    # The caller supplies a GUI in its private disposable package clone.
    foreach ($window in @(Own-Gui)) {
        # The UI cancels WM_CLOSE while its initial dependency check is busy.
        # Retry after it becomes idle instead of treating that protection as a failure.
        for ($i=0; $i -lt 75 -and -not $window.HasExited; $i++) {
            $window.CloseMainWindow() | Out-Null
            if ($window.WaitForExit(200)) { break }
        }
        if (-not $window.HasExited) { $window.Kill(); throw "Owned GUI test instance did not close." }
        $window.Dispose()
    }
}
function Assert-NoGui {
    if (@(Own-Gui).Count) { throw "A CLI or protocol request unexpectedly opened a GUI." }
}
function Launch-AndCheck([string]$File, [string]$Arguments, [bool]$Shell, [string]$Name) {
    Write-Host "Checking GUI launch: $Name"
    Assert-NoGui
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $File; $start.Arguments = $Arguments
    $start.WorkingDirectory = $Directory; $start.UseShellExecute = $Shell
    if (-not $Shell) {
        $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = $utf8; $start.StandardErrorEncoding = $utf8
    }
    $process = $null
    try {
        $process = [Diagnostics.Process]::Start($start)
        if ($process) {
            if (-not $Shell) {
                $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
            }
            if (-not $process.WaitForExit(15000)) { $process.Kill(); throw "GUI launcher should exit after spawning its frontend: $Name" }
            if ($process.ExitCode -ne 0) { throw "Launcher failed: $Name ($($process.ExitCode))" }
            if (-not $Shell -and (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000))) { throw "Launcher leaked redirected streams into the GUI: $Name" }
            if (-not $Shell -and ($stdout.Result -or $stderr.Result)) { throw "GUI launch polluted CLI streams: $Name" }
        }
        $window = $null
        for ($i=0; $i -lt 50; $i++) {
            $window = @(Own-Gui | Where-Object { $_.MainWindowHandle -ne 0 })
            if ($window.Count -eq 1) { break }
            Start-Sleep -Milliseconds 200
        }
        if ($window.Count -ne 1) { throw "Expected one native GUI window: $Name" }
        $checks.Add($Name)
        Write-Host "[OK] GUI launch: $Name"
    } finally {
        Close-OwnGui
        if ($process) { $process.Dispose() }
    }
}
try {
    # CreateProcess, Windows shell, cmd, PowerShell and a shell shortcut all
    # provide argv[0] differently; none should need an extra user argument.
    Launch-AndCheck $Backend "" $false "direct_process_without_arguments"
    Launch-AndCheck $Backend "" $true "windows_shell_double_click_launch_path"
    Launch-AndCheck $env:ComSpec ('/d /s /c ""' + $Backend + '""') $false "cmd_quoted_unicode_path"
    $psScript = Join-Path $Directory "launch.ps1"
    [IO.File]::WriteAllText($psScript, '& $args[0]' + "`r`nexit `$LASTEXITCODE`r`n", $utf8)
    $arguments = (@("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $psScript, $Backend) | ForEach-Object { Quote-Argument $_ }) -join ' '
    Launch-AndCheck (Join-Path $PSHOME "powershell.exe") $arguments $false "powershell_quoted_unicode_path"
    $shortcut = Join-Path $Directory "xEtch shortcut.lnk"
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $Backend; $link.WorkingDirectory = $Directory; $link.Arguments = ""; $link.Save()
    Launch-AndCheck $shortcut "" $true "shortcut_with_unrelated_working_directory"
    foreach ($case in @(
        @{arguments=@("--help");pattern="Usage:"},
        @{arguments=@("--version");pattern="xetch \d+\.\d+\.\d+"},
        @{arguments=@("--json", "doctor");pattern='"protocol_version":1'}
    )) {
        $text = Invoke-Tool $Backend $case.arguments -WorkingDirectory $Directory
        if ($text -notmatch $case.pattern) { throw "CLI output changed: $($case.arguments)" }
        Assert-NoGui
    }
    $checks.Add("help_version_and_json_cli_do_not_open_gui")
    foreach ($arguments in @(@("--unknown"), @("--json"), @("serve"), @("-psn_0_123"), @("-Embedding"))) {
        $text = Invoke-Tool $Backend $arguments -WorkingDirectory $Directory -ExpectFailure -IncludeStandardError
        if ($text -notmatch "error:|Usage:") { throw "CLI parser did not reject arguments: $($arguments -join ' ')" }
        Assert-NoGui
    }
    $checks.Add("unknown_incomplete_and_non_windows_metadata_arguments_are_rejected")
    $request = '{"jsonrpc":"2.0","id":73,"method":"system.capabilities"}' + "`n"
    $response = Invoke-Tool $Backend @("serve", "--stdio") -StdinText $request -WorkingDirectory $Directory | ConvertFrom-Json
    if ($response.id -ne 73 -or $response.result.protocol_version -ne 1) { throw "Backend stdio protocol was changed." }
    Assert-NoGui
    $checks.Add("stdio_backend_response_is_clean_json_and_exits_on_eof")
    Move-Item -LiteralPath $Gui -Destination ($Gui + ".hidden")
    try {
        $text = Invoke-Tool $Backend @() -WorkingDirectory $Directory -ExpectFailure -IncludeStandardError
        if ($text -notmatch "GUI was not found") { throw "Missing frontend diagnostic was not reported." }
        Invoke-Tool $Backend @("--version") -WorkingDirectory $Directory | Out-Null
        Assert-NoGui
        $checks.Add("missing_packaged_gui_reports_error_without_source_tree_fallback")
    } finally { Move-Item -LiteralPath ($Gui + ".hidden") -Destination $Gui }
    $report.status = "pass"
} catch { $report.status = "fail"; $report.error = $_.ToString(); Write-Host $report.error }
finally { Close-OwnGui }
$path = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
[IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 8), $utf8)
if ($report.status -ne "pass") { throw $report.error }
Write-Host "PASS native Windows backend launcher, shell/shortcut startup and CLI/RPC routing"
