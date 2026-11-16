using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace XEtch.Windows;

/// <summary>The GUI owns presentation only; all disk access goes through the CLI's JSON-RPC API.</summary>
public sealed class BackendClient : IAsyncDisposable
{
    private readonly Process _process;
    private readonly SemaphoreSlim _gate = new(1);
    private readonly Task _stderr;
    private readonly StringBuilder _diagnostics = new();
    private long _id;
    private bool _disposed;
    public string ProgramPath { get; }

    public BackendClient(string? program = null, IReadOnlyList<string>? arguments = null)
    {
        ProgramPath = program ?? FindBackend();
        var start = new ProcessStartInfo(ProgramPath)
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false),
            StandardOutputEncoding = new UTF8Encoding(false), StandardErrorEncoding = new UTF8Encoding(false)
        };
        foreach (var argument in arguments ?? new[] { "serve", "--stdio" }) start.ArgumentList.Add(argument);
        var project = FindProject();
        foreach (var (variable, name) in new[] { ("XETCH_QEMU_IMG", "qemu-img.exe"), ("XETCH_QEMU_STORAGE_DAEMON", "qemu-storage-daemon.exe") })
        {
            if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable(variable))) continue;
            var candidates = new[] { Path.Combine(Path.GetDirectoryName(ProgramPath)!, "qemu", name),
                project is null ? "" : Path.Combine(project, "artifacts", "windows", "qemu", name) };
            var found = candidates.FirstOrDefault(File.Exists);
            if (found is not null) start.Environment[variable] = found;
        }
        _process = Process.Start(start) ?? throw new IOException("无法启动 xEtch 后端。");
        _stderr = ReadDiagnosticsAsync();
    }

    private static string? FindProject()
    {
        if (File.Exists(Path.Combine(AppContext.BaseDirectory, "..", "xetch-package.json"))) return null;
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
            if (File.Exists(Path.Combine(dir.FullName, "Cargo.toml"))) return dir.FullName;
        return null;
    }

    private static string FindBackend()
    {
        var configured = Environment.GetEnvironmentVariable("XETCH_BACKEND");
        if (!string.IsNullOrWhiteSpace(configured)) return Path.GetFullPath(configured);
        var project = FindProject();
        var package = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, ".."));
        var candidates = new[] { Path.Combine(AppContext.BaseDirectory, "backend", "xetch.exe"),
            Path.Combine(AppContext.BaseDirectory, "xetch.exe"),
            File.Exists(Path.Combine(package, "xetch-package.json")) ? Path.Combine(package, "xetch.exe") : "",
            File.Exists(Path.Combine(package, "build.json")) ? Path.Combine(package, "backend", "xetch.exe") : "",
            project is null ? "" : Path.Combine(project, "target", "windows", "release", "xetch.exe"),
            project is null ? "" : Path.Combine(project, "target", "release", "xetch.exe") };
        return candidates.FirstOrDefault(File.Exists)
            ?? throw new FileNotFoundException("找不到 xetch.exe。请构建后端，或设置 XETCH_BACKEND 为它的完整路径。");
    }

    private async Task ReadDiagnosticsAsync()
    {
        var buffer = new char[1024];
        int count;
        while ((count = await _process.StandardError.ReadAsync(buffer)) > 0)
            lock (_diagnostics)
            {
                _diagnostics.Append(buffer, 0, count);
                if (_diagnostics.Length > 8192) _diagnostics.Remove(0, _diagnostics.Length - 8192);
            }
    }

    public async Task<JsonElement> CallAsync(string method, object parameters)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        await _gate.WaitAsync();
        try
        {
            var id = ++_id;
            var request = JsonSerializer.Serialize(new { jsonrpc = "2.0", id, method, @params = parameters });
            if (Encoding.UTF8.GetByteCount(request) >= 1024 * 1024) throw new ArgumentException("请求超过 1 MiB。");
            await _process.StandardInput.WriteLineAsync(request);
            await _process.StandardInput.FlushAsync();
            var line = await _process.StandardOutput.ReadLineAsync();
            if (line is null)
            {
                string detail;
                lock (_diagnostics) detail = _diagnostics.ToString();
                throw new IOException($"后端已退出。可重新打开编辑会话恢复已完成的更改。{detail}");
            }
            using var document = JsonDocument.Parse(line);
            var response = document.RootElement;
            if (response.GetProperty("jsonrpc").GetString() != "2.0" || response.GetProperty("id").GetInt64() != id)
                throw new IOException("后端返回的响应与请求不匹配。");
            if (response.TryGetProperty("error", out var error))
            {
                var message = error.GetProperty("message").GetString() ?? "未知错误";
                throw new BackendException(message);
            }
            return response.GetProperty("result").Clone();
        }
        finally { _gate.Release(); }
    }

    public async ValueTask DisposeAsync()
    {
        if (_disposed) return;
        _disposed = true;
        _process.StandardInput.Close();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
        try { await _process.WaitForExitAsync(deadline.Token); }
        catch (OperationCanceledException) { if (!_process.HasExited) _process.Kill(entireProcessTree: true); }
        await _stderr;
        _process.Dispose();
        _gate.Dispose();
    }
}

public sealed class BackendException(string message) : Exception(message);
