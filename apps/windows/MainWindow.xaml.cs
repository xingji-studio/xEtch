using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.Windows.Storage.Pickers;
using System.Collections.ObjectModel;
using System.Text.Json;

namespace XEtch.Windows;

public sealed partial class MainWindow : Window
{
    private BackendClient? _backend;
    private readonly ObservableCollection<FileItem> _files = [];
    private readonly HashSet<string> _readable = [];
    private string? _image, _workspace;
    private string _directory = "/";
    private bool _busy, _loaded, _choosingPartition, _dependencies, _ntfsWrite;
    private PartitionItem? Partition => Partitions.SelectedItem as PartitionItem;
    private FileItem? Selection => Files.SelectedItem as FileItem;
    private bool CanRead => _workspace is not null && Partition is { } p && _readable.Contains(p.FileSystem);
    private bool CanWrite => CanRead && Partition is { } p &&
        (p.FileSystem.StartsWith("FAT", StringComparison.Ordinal) || p.FileSystem == "NTFS" && _ntfsWrite ||
        (p.FileSystem is "exFAT" or "ext4") && ExperimentalToggle.IsChecked == true) &&
        !(p.FileSystem == "NTFS" && _directory.Split('/').Any(s => s.StartsWith('$')));

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        AppWindow.Resize(new global::Windows.Graphics.SizeInt32(1280, 880));
        Files.ItemsSource = _files;
        Root.Loaded += InitializeAsync;
        AppWindow.Closing += (_, args) =>
        {
            if (_busy) { args.Cancel = true; Status.Text = "操作正在进行，请完成后再关闭窗口。"; }
        };
        Closed += async (_, _) =>
        {
            if (_backend is not null)
                try { await _backend.DisposeAsync(); } catch (Exception) { /* Window is already closed. */ }
        };
    }

    private async void InitializeAsync(object sender, RoutedEventArgs args)
    {
        if (_loaded) return;
        _loaded = true;
        await RunAsync("正在检查运行环境…", async () =>
        {
            _backend = new BackendClient();
            var result = await Call("system.capabilities", new { });
            var tools = result.GetProperty("tools").EnumerateArray().ToArray();
            _dependencies = tools.All(t => t.GetProperty("available").GetBoolean());
            var capabilities = result.GetProperty("capabilities");
            _ntfsWrite = capabilities.GetProperty("ntfs_write").GetBoolean();
            foreach (var fs in capabilities.GetProperty("filesystem_read").EnumerateArray()) _readable.Add(fs.GetString()!);
            EngineStatus.Text = _ntfsWrite ? "FAT · NTFS 读写就绪" : "FAT 读写就绪 · NTFS 只读";
            if (!_dependencies)
                throw new IOException("未找到 QEMU 工具。请准备 qemu-img 和 qemu-storage-daemon，或设置 XETCH_QEMU_IMG / XETCH_QEMU_STORAGE_DAEMON。");
            var commandLine = Environment.GetCommandLineArgs();
            if (commandLine.Length == 3 && commandLine[1] == "--image") await OpenImageAsync(commandLine[2]);
            if (commandLine.Length == 3 && commandLine[1] == "--workspace") await ResumeAsync(commandLine[2]);
        });
        OnReadyForTests();
    }

    partial void OnReadyForTests();

    private Task<JsonElement> Call(string method, object args) =>
        (_backend ?? throw new IOException("后端尚未启动。")).CallAsync(method, args);

    private async Task RunAsync(string status, Func<Task> action)
    {
        if (_busy) return;
        _busy = true;
        Notice.IsOpen = false;
        Status.Text = status;
        BusyRing.IsActive = true;
        UpdateControls();
        try
        {
            await action();
            Status.Text = _workspace is null ? "就绪" : "就绪 · 编辑会话已保留";
        }
        catch (Exception error)
        {
            Notice.Title = "未能完成操作";
            Notice.Message = error.Message;
            Notice.Severity = InfoBarSeverity.Error;
            Notice.IsOpen = true;
            Status.Text = "操作未完成；可查看错误原因后重试。";
            DirectoryPath.Text = _directory;
        }
        finally
        {
            _busy = false;
            BusyRing.IsActive = false;
            UpdateControls();
        }
    }

    private void UpdateControls()
    {
        if (!_loaded) return;
        OpenButton.IsEnabled = ResumeButton.IsEnabled = CreateButton.IsEnabled = !_busy && _dependencies;
        Partitions.IsEnabled = !_busy && _workspace is not null;
        SaveImageButton.IsEnabled = PlanButton.IsEnabled = !_busy && _workspace is not null;
        UpButton.IsEnabled = RefreshButton.IsEnabled = DirectoryPath.IsEnabled = Files.IsEnabled = !_busy && CanRead;
        FileCommands.IsEnabled = !_busy && CanRead;
        ExperimentalToggle.IsEnabled = !_busy;
        ImportButton.IsEnabled = MkdirButton.IsEnabled = !_busy && CanWrite;
        GetButton.IsEnabled = !_busy && CanRead && Selection is { IsDirectory: false };
        bool editable = !_busy && CanWrite && Selection is { } file &&
            !(Partition?.FileSystem == "NTFS" && file.Name.StartsWith('$'));
        RemoveButton.IsEnabled = editable;
        RenameButton.IsEnabled = editable &&
            !(Partition?.FileSystem == "NTFS" && Selection!.IsDirectory);
        AccessText.Text = Partition is null ? "" : CanWrite ? "可以编辑 · 更改在另存后生效" :
            Partition.FileSystem is "exFAT" or "ext4" ? "浏览模式 · 写入需开启实验选项" : "浏览模式";
    }

    private static string NormalizeDirectory(string path)
    {
        path = path.Trim();
        if (!path.StartsWith('/')) path = "/" + path;
        return path == "/" ? "/" : path.TrimEnd('/');
    }
    private string ChildPath(string name) => _directory == "/" ? "/" + name : _directory + "/" + name;

    private async Task RefreshFilesAsync(string? directory = null)
    {
        if (!CanRead)
        {
            _files.Clear();
            EmptyTitle.Text = "此卷暂不支持浏览";
            EmptyDescription.Text = "可切换到其他分区，或将整个镜像另存。";
            EmptyState.Visibility = Visibility.Visible;
            return;
        }
        var requested = NormalizeDirectory(directory ?? _directory);
        var result = await Call("file.list", new { workspace = _workspace, partition = Partition!.Index, path = requested });
        var entries = result.GetProperty("entries").EnumerateArray().Select(e => new FileItem(
            e.GetProperty("name").GetString()!, e.GetProperty("is_directory").GetBoolean(), e.GetProperty("size").GetUInt64()))
            .OrderByDescending(e => e.IsDirectory).ThenBy(e => e.Name, StringComparer.CurrentCultureIgnoreCase).ToList();
        _directory = requested;
        DirectoryPath.Text = requested;
        _files.Clear();
        foreach (var entry in entries) _files.Add(entry);
        EmptyTitle.Text = "此文件夹为空";
        EmptyDescription.Text = CanWrite ? "导入文件，或新建文件夹。" : "可以切换路径或浏览其他分区。";
        EmptyState.Visibility = entries.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        EntryCount.Text = $"{entries.Count} 个项目";
    }

    private async Task RefreshPlanAsync()
    {
        var plan = await Call("workspace.plan", new { workspace = _workspace });
        ChangeCount.Text = $"{plan.GetProperty("manifest").GetProperty("changes").GetArrayLength()} 项会话更改";
    }

    private async Task ShowImageAsync(string image, string workspace, JsonElement inspection)
    {
        _image = image; _workspace = workspace; _directory = "/";
        ImageName.Text = Path.GetFileName(image);
        ToolTipService.SetToolTip(ImageName, image);
        var info = inspection.GetProperty("image");
        ImageDetails.Text = $"{info.GetProperty("format").GetString()!.ToUpperInvariant()} · {FileItem.FormatSize(info.GetProperty("virtual_size").GetUInt64())}";
        var partitions = inspection.GetProperty("layout").GetProperty("partitions").EnumerateArray().Select(p => new PartitionItem(
            p.GetProperty("index").GetUInt32(), p.GetProperty("filesystem").GetString()!, p.GetProperty("size").GetUInt64(), p.GetProperty("name").GetString()!)).ToList();
        _choosingPartition = true;
        Partitions.ItemsSource = partitions;
        Partitions.SelectedItem = partitions.FirstOrDefault(p => _readable.Contains(p.FileSystem)) ?? partitions.FirstOrDefault();
        _choosingPartition = false;
        await RefreshPlanAsync();
        await RefreshFilesAsync("/");
    }

    private async Task OpenImageAsync(string image)
    {
        image = Path.GetFullPath(image);
        var inspection = await Call("image.inspect", new { image });
        var parent = Environment.GetEnvironmentVariable("XETCH_WORKSPACE_ROOT") ??
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "xEtch", "Workspaces");
        Directory.CreateDirectory(parent);
        var workspace = Path.Combine(parent, $"edit-{DateTime.UtcNow:yyyyMMdd-HHmmss}-{Guid.NewGuid():N}");
        await Call("workspace.create", new { image, workspace });
        await ShowImageAsync(image, workspace, inspection);
    }

    private async Task ResumeAsync(string workspace)
    {
        var plan = await Call("workspace.plan", new { workspace });
        var image = plan.GetProperty("manifest").GetProperty("source").GetProperty("path").GetString()!;
        var inspection = await Call("image.inspect", new { image });
        await ShowImageAsync(image, workspace, inspection);
    }

    private async Task<string?> PickOpenAsync(bool images)
    {
        var picker = new FileOpenPicker(AppWindow.Id);
        if (images) { picker.FileTypeFilter.Add(".img"); picker.FileTypeFilter.Add(".vmdk"); picker.FileTypeFilter.Add(".raw"); }
        else picker.FileTypeFilter.Add("*");
        return (await picker.PickSingleFileAsync())?.Path;
    }

    private async Task<string?> PickSaveAsync(string name, bool image)
    {
        var picker = new FileSavePicker(AppWindow.Id) { SuggestedFileName = name };
        if (image)
        {
            picker.FileTypeChoices.Add("VMware 镜像", new List<string> { ".vmdk" });
            picker.FileTypeChoices.Add("原始磁盘镜像", new List<string> { ".img" });
        }
        else picker.FileTypeChoices.Add("文件", new List<string> { Path.GetExtension(name) is { Length: > 0 } extension ? extension : ".bin" });
        var chosen = (await picker.PickSaveFileAsync())?.Path;
        if (chosen is not null && (File.Exists(chosen) || Directory.Exists(chosen)))
            throw new IOException("目标已存在，请选择新的文件名。原有文件不会被覆盖。");
        return chosen;
    }

    private ContentDialog Dialog(string title, object content, string primary) => new()
    {
        XamlRoot = Root.XamlRoot, Title = title, Content = content, PrimaryButtonText = primary,
        CloseButtonText = "取消", DefaultButton = ContentDialogButton.Close
    };

    private async Task<string?> NameAsync(string title, string value = "")
    {
        var field = new TextBox { Text = value, MinWidth = 340, PlaceholderText = "输入名称" };
        field.Loaded += (_, _) => { field.Focus(FocusState.Programmatic); field.SelectAll(); };
        var dialog = Dialog(title, field, "确定");
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return null;
        var name = field.Text;
        if (string.IsNullOrWhiteSpace(name) || name is "." or ".." || name.IndexOfAny(['/', '\\']) >= 0)
            throw new ArgumentException("请输入有效的单个文件或文件夹名称。");
        return name;
    }

    private async Task MutateAsync(string operation, string path, string? source = null, string? destination = null, bool replace = false)
    {
        await Call("file." + operation, new { workspace = _workspace, partition = Partition!.Index, path, source, destination,
            replace, experimental_filesystems = ExperimentalToggle.IsChecked == true });
        await RefreshPlanAsync();
        await RefreshFilesAsync();
    }

    private async void Open_Click(object sender, RoutedEventArgs args) => await RunAsync("正在打开镜像…", async () =>
    { if (await PickOpenAsync(true) is { } path) await OpenImageAsync(path); });

    private async void Resume_Click(object sender, RoutedEventArgs args) => await RunAsync("正在恢复编辑会话…", async () =>
    {
        var folder = await new FolderPicker(AppWindow.Id).PickSingleFolderAsync();
        if (folder is not null) await ResumeAsync(folder.Path);
    });

    private async void Partition_Changed(object sender, SelectionChangedEventArgs args)
    {
        if (_choosingPartition || _workspace is null || _busy) return;
        await RunAsync("正在读取分区…", () => RefreshFilesAsync("/"));
    }
    private void Experimental_Changed(object sender, RoutedEventArgs args) { if (_loaded) UpdateControls(); }
    private void FileSelection_Changed(object sender, SelectionChangedEventArgs args) => UpdateControls();
    private async void File_Click(object sender, ItemClickEventArgs args)
    {
        if (args.ClickedItem is FileItem { IsDirectory: true } item)
            await RunAsync("正在读取目录…", () => RefreshFilesAsync(ChildPath(item.Name)));
    }
    private async void Up_Click(object sender, RoutedEventArgs args) => await RunAsync("正在读取目录…", () =>
        RefreshFilesAsync(_directory == "/" || _directory.LastIndexOf('/') <= 0 ? "/" : _directory[.._directory.LastIndexOf('/')]));
    private async void Refresh_Click(object sender, RoutedEventArgs args) => await RunAsync("正在刷新…", async () =>
    { await RefreshFilesAsync(); await RefreshPlanAsync(); });
    private async void Path_KeyDown(object sender, KeyRoutedEventArgs args)
    {
        if (args.Key != global::Windows.System.VirtualKey.Enter) return;
        args.Handled = true;
        await RunAsync("正在读取目录…", () => RefreshFilesAsync(DirectoryPath.Text));
    }

    private async void Import_Click(object sender, RoutedEventArgs args) => await RunAsync("正在导入文件…", async () =>
    {
        if (await PickOpenAsync(false) is not { } source) return;
        var name = Path.GetFileName(source);
        bool replace = _files.Any(e => string.Equals(e.Name, name, StringComparison.OrdinalIgnoreCase));
        if (replace && await Dialog("替换镜像中的文件？", $"将用所选文件替换 {ChildPath(name)}。", "替换").ShowAsync() != ContentDialogResult.Primary) return;
        await MutateAsync("put", ChildPath(name), source: source, replace: replace);
    });
    private async void Mkdir_Click(object sender, RoutedEventArgs args) => await RunAsync("正在创建文件夹…", async () =>
    { if (await NameAsync("新建文件夹") is { } name) await MutateAsync("mkdir", ChildPath(name)); });
    private async void Rename_Click(object sender, RoutedEventArgs args) => await RunAsync("正在重命名…", async () =>
    {
        if (Selection is not { } item) return;
        if (await NameAsync("重命名", item.Name) is { } name && name != item.Name)
            await MutateAsync("rename", ChildPath(item.Name), destination: ChildPath(name));
    });
    private async void Remove_Click(object sender, RoutedEventArgs args) => await RunAsync("正在删除…", async () =>
    {
        if (Selection is not { } item) return;
        if (await Dialog("从镜像中删除？", $"删除 {ChildPath(item.Name)}。文件夹必须为空。", "删除").ShowAsync() == ContentDialogResult.Primary)
            await MutateAsync("remove", ChildPath(item.Name));
    });
    private async void Get_Click(object sender, RoutedEventArgs args) => await RunAsync("正在导出文件…", async () =>
    {
        if (Selection is not { IsDirectory: false } item) return;
        if (await PickSaveAsync(item.Name, false) is { } destination)
            await Call("file.get", new { workspace = _workspace, partition = Partition!.Index, path = ChildPath(item.Name), destination });
    });
    private async void SaveImage_Click(object sender, RoutedEventArgs args) => await RunAsync("正在保存镜像，请稍候…", async () =>
    {
        if (await PickSaveAsync(Path.GetFileNameWithoutExtension(_image!) + "-edited.vmdk", true) is not { } output) return;
        await Call("workspace.apply", new { workspace = _workspace, output });
        Notice.Title = "镜像已保存";
        Notice.Message = output;
        Notice.Severity = InfoBarSeverity.Success;
        Notice.IsOpen = true;
    });

    private async void Plan_Click(object sender, RoutedEventArgs args) => await RunAsync("正在读取更改…", async () =>
    {
        var plan = await Call("workspace.plan", new { workspace = _workspace });
        var changes = plan.GetProperty("manifest").GetProperty("changes").EnumerateArray().Select(c =>
        {
            var operation = c.GetProperty("operation").GetString() switch
            { "file.put" => "导入 / 替换", "file.mkdir" => "新建文件夹", "file.remove" => "删除", "file.rename" => "重命名", var other => other };
            var destination = c.TryGetProperty("destination", out var to) ? " → " + to.GetString() : "";
            return $"{operation}  {c.GetProperty("path").GetString()}{destination}";
        });
        var text = string.Join("\n", changes);
        var panel = new StackPanel { Spacing = 16, MaxWidth = 600 };
        panel.Children.Add(new TextBlock { Text = "会话位置（恢复时选择此文件夹）", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        panel.Children.Add(new TextBox { Text = _workspace, IsReadOnly = true, TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(new TextBlock { Text = text.Length == 0 ? "还没有文件更改。" : text, TextWrapping = TextWrapping.Wrap });
        await new ContentDialog { XamlRoot = Root.XamlRoot, Title = "编辑会话的更改", Content = new ScrollViewer { Content = panel, MaxHeight = 400 }, CloseButtonText = "关闭" }.ShowAsync();
    });

    private string[] NewFileSystems => ExperimentalToggle.IsChecked == true
        ? ["fat32", "exfat", "ext4"] : ["fat32"];

    private (StackPanel Panel, NumberBox Size, ComboBox Table, ComboBox FileSystem) CreateImageForm()
    {
        var size = new NumberBox { Header = "容量（MiB）", Value = 64, Minimum = 64, Maximum = 1048576, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact };
        var table = new ComboBox { Header = "分区表", ItemsSource = new[] { "GPT", "MBR", "独立卷" }, SelectedIndex = 0, HorizontalAlignment = HorizontalAlignment.Stretch };
        var filesystem = new ComboBox { Header = "文件系统", ItemsSource = NewFileSystems.Select(fs => fs switch { "fat32" => "FAT32", "exfat" => "exFAT（实验）", _ => "ext4（实验）" }).ToArray(), SelectedIndex = 0, HorizontalAlignment = HorizontalAlignment.Stretch };
        var panel = new StackPanel { Spacing = 16, MinWidth = 340 };
        panel.Children.Add(new TextBlock { Text = "开启实验写入后可新建 exFAT 和 ext4 镜像。现有 NTFS 镜像可直接打开。", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(size); panel.Children.Add(table); panel.Children.Add(filesystem);
        return (panel, size, table, filesystem);
    }

    private async Task CreateImageAsync(string output, double sizeMiB, int tableIndex, int filesystemIndex)
    {
        if (!double.IsFinite(sizeMiB) || sizeMiB < 64 || sizeMiB > 1048576 || sizeMiB != Math.Truncate(sizeMiB))
            throw new ArgumentException("容量必须为 64 至 1048576 之间的整数 MiB。");
        if (filesystemIndex < 0 || filesystemIndex >= NewFileSystems.Length || tableIndex is < 0 or > 2)
            throw new ArgumentException("请选择有效的文件系统和分区表。");
        await Call("image.create", new { output, size = checked((ulong)sizeMiB * 1024 * 1024), table = tableIndex switch { 0 => "gpt", 1 => "mbr", _ => "none" }, filesystem = NewFileSystems[filesystemIndex], experimental_filesystems = ExperimentalToggle.IsChecked == true });
        await OpenImageAsync(output);
    }

    private async void Create_Click(object sender, RoutedEventArgs args) => await RunAsync("正在新建镜像…", async () =>
    {
        var form = CreateImageForm();
        if (await Dialog("新建镜像", form.Panel, "选择保存位置").ShowAsync() != ContentDialogResult.Primary) return;
        if (await PickSaveAsync("new-disk.img", true) is not { } output) return;
        await CreateImageAsync(output, form.Size.Value, form.Table.SelectedIndex, form.FileSystem.SelectedIndex);
    });
}
