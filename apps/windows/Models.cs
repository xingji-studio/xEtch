namespace XEtch.Windows;

public sealed record PartitionItem(uint Index, string FileSystem, ulong Size, string Name)
{
    public string Label => $"{(Index == 0 ? "卷" : $"分区 {Index}")} · {FileSystem} · {FileItem.FormatSize(Size)}";
}

public sealed record FileItem(string Name, bool IsDirectory, ulong Size)
{
    public string Icon => IsDirectory ? "\uE8B7" : "\uE8A5";
    public string Kind => IsDirectory ? "文件夹" : "文件";
    public string SizeText => IsDirectory ? "—" : FormatSize(Size);
    public static string FormatSize(ulong size)
    {
        string[] units = ["B", "KiB", "MiB", "GiB", "TiB"];
        double value = size;
        var unit = 0;
        while (value >= 1024 && unit < units.Length - 1) { value /= 1024; unit++; }
        return unit == 0 ? $"{size} B" : $"{value:0.##} {units[unit]}";
    }
}
