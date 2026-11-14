$utf8 = [System.Text.UTF8Encoding]::new($false)
# Use ProcessStartInfo so that redirected UTF-8 works consistently in PS 5.1
# and PS 7, including native programs launched from WSL.
function Quote-Argument([string]$Value) {
    return '"' + [regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function Invoke-Tool([string]$Program, [string[]]$ToolArguments, [string]$StdinText = "", [switch]$ExpectFailure, [string]$WorkingDirectory = "", [switch]$IncludeStandardError) {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Program
    if ($WorkingDirectory) { $start.WorkingDirectory = $WorkingDirectory }
    $start.Arguments = ($ToolArguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.RedirectStandardInput = $true
    $start.StandardOutputEncoding = $utf8
    $start.StandardErrorEncoding = $utf8
    $process = [System.Diagnostics.Process]::Start($start)
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if ($StdinText) {
        $stdinBytes = $utf8.GetBytes($StdinText)
        $process.StandardInput.BaseStream.Write($stdinBytes, 0, $stdinBytes.Length)
        $process.StandardInput.BaseStream.Flush()
    }
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(180000)) {
        $process.Kill()
        throw "Timed out: $Program $($start.Arguments)"
    }
    $stdout = $stdoutTask.Result
    $stderr = $stderrTask.Result
    $exitCode = $process.ExitCode
    $process.Dispose()
    if ($ExpectFailure) {
        if ($exitCode -eq 0) { throw "Expected failure: $Program $($start.Arguments)" }
    } elseif ($exitCode -ne 0) {
        throw "Exit ${exitCode}: $Program $($start.Arguments)`n$stdout`n$stderr"
    }
    if ($IncludeStandardError) { return $stdout + $stderr }
    return $stdout
}
