// Compiled ONLY for the native GUI integration test build. This exercises our
// own controller and renders its actual WinUI visual tree; it does not automate
// other Windows apps, file pickers or input events.
#if XETCH_GUI_SMOKE
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using System.Security.Cryptography;

namespace XEtch.Windows;

public sealed partial class MainWindow
{
    partial void OnReadyForTests()
    {
        var args = Environment.GetCommandLineArgs();
        if (args.Length == 3 && args[1] == "--smoke-test") _ = SmokeAsync(args[2]);
    }

    private async Task SmokeAsync(string configuration)
    {
        using var config = JsonDocument.Parse(await File.ReadAllTextAsync(configuration));
        if (config.RootElement.TryGetProperty("device_fixture", out var deviceFixture) && deviceFixture.GetBoolean())
        {
            await SmokeDeviceAsync(config.RootElement);
            return;
        }
        string Field(string key) => config.RootElement.GetProperty(key).GetString()!;
        var filesystem = config.RootElement.TryGetProperty("filesystem", out var fs) ? fs.GetString()! : "NTFS";
        var create = config.RootElement.TryGetProperty("create_image", out var createField) && createField.GetBoolean();
        var table = config.RootElement.TryGetProperty("table", out var tableField) ? tableField.GetString()! : "gpt";
        string? originalHash = null;
        bool sourceUnchanged = false;
        var checks = new List<string>();
        string? error = null;
        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException(name);
            checks.Add(name);
        }
        try
        {
            Check(_dependencies && (filesystem != "NTFS" || _ntfsWrite), "native_window_loaded_and_dependencies_ready");
            await RunAsync("正在验证界面与后端…", async () =>
            {
                if (create)
                {
                    Check(NewFileSystems.SequenceEqual(new[] { "fat32" }), "new_experimental_filesystems_hidden_until_opt_in");
                    ExperimentalToggle.IsChecked = true;
                    var form = CreateImageForm();
                    form.Table.SelectedIndex = table switch { "gpt" => 0, "mbr" => 1, _ => 2 };
                    form.FileSystem.SelectedIndex = Array.IndexOf(NewFileSystems, filesystem.ToLowerInvariant());
                    Check(form.FileSystem.SelectedIndex >= 0 && form.FileSystem.Items.Count == 3,
                        "new_filesystem_available_in_same_form_as_create_dialog");
                    await CreateImageAsync(Field("image"), form.Size.Value, form.Table.SelectedIndex, form.FileSystem.SelectedIndex);
                    Check(File.Exists(Field("image")), "gui_created_image_and_opened_editing_session");
                    ExperimentalToggle.IsChecked = false;
                }
                else await OpenImageAsync(Field("image"));
                using (var input = File.OpenRead(Field("image"))) originalHash = Convert.ToHexString(await SHA256.HashDataAsync(input));
                Check(Partition?.FileSystem == filesystem, "expected_partition_presented");
                if (filesystem is "exFAT" or "ext4")
                {
                    Check(!CanWrite, "experimental_filesystem_initially_read_only");
                    ExperimentalToggle.IsChecked = true;
                    Check(CanWrite, "experimental_toggle_enables_filesystem_edits");
                }
                await MutateAsync("mkdir", "/gui-created");
                await RefreshFilesAsync("/gui-created");
                await MutateAsync("put", "/gui-created/内容.txt", source: Field("payload"));
                Check(_files.Any(f => f.Name == "内容.txt"), "import_refreshes_visible_file_list");
                await MutateAsync("rename", "/gui-created/内容.txt", destination: "/gui-created/renamed.txt");
                await Call("file.get", new { workspace = _workspace, partition = Partition!.Index,
                    path = "/gui-created/renamed.txt", destination = Field("readback") });
                Check((await File.ReadAllBytesAsync(Field("readback"))).SequenceEqual(await File.ReadAllBytesAsync(Field("payload"))), "client_file_export_bytes_match");
                await Call("workspace.apply", new { workspace = _workspace, output = Field("output") });
                var previous = _workspace!;
                await ResumeAsync(previous);
                await RefreshFilesAsync("/gui-created");
                Check(_files.Any(f => f.Name == "renamed.txt"), "persistent_session_resume_restores_view");
            });
            Check(!Notice.IsOpen, "gui_edit_and_export_completed_without_error");
            Check(ImportButton.IsEnabled && SaveImageButton.IsEnabled && !BusyRing.IsActive, "controls_reenabled_after_async_work");
            Files.SelectedItem = _files.Single(f => f.Name == "renamed.txt");
            UpdateControls();
            Check(RenameButton.IsEnabled, "rename_action_available_for_selected_regular_file");
            var before = await File.ReadAllTextAsync(Path.Combine(_workspace!, "workspace.json"));
            await RunAsync("正在验证错误提示…", () => MutateAsync("put", "/gui-created/renamed.txt", source: Field("payload")));
            Check(Notice.IsOpen && Notice.Severity == InfoBarSeverity.Error, "backend_error_is_visible_in_infobar");
            Check(before == await File.ReadAllTextAsync(Path.Combine(_workspace!, "workspace.json")), "failed_gui_edit_does_not_advance_checkpoint");
            await RunAsync("正在刷新…", () => RefreshFilesAsync());
            Check(!Notice.IsOpen && ImportButton.IsEnabled, "gui_recovers_after_rejected_operation");
            using (var input = File.OpenRead(Field("image")))
                sourceUnchanged = originalHash == Convert.ToHexString(await SHA256.HashDataAsync(input));
            Check(sourceUnchanged, "source_image_unchanged_after_gui_edits");
            // Render after the dispatcher has applied layout, not an HTML mockup.
            await Task.Delay(500);
            var bitmap = new RenderTargetBitmap();
            await bitmap.RenderAsync(Root);
            var pixels = await bitmap.GetPixelsAsync();
            var folder = await global::Windows.Storage.StorageFolder.GetFolderFromPathAsync(Path.GetDirectoryName(Field("screenshot"))!);
            var file = await folder.CreateFileAsync(Path.GetFileName(Field("screenshot")), global::Windows.Storage.CreationCollisionOption.FailIfExists);
            using var stream = await file.OpenAsync(global::Windows.Storage.FileAccessMode.ReadWrite);
            var encoder = await global::Windows.Graphics.Imaging.BitmapEncoder.CreateAsync(global::Windows.Graphics.Imaging.BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(global::Windows.Graphics.Imaging.BitmapPixelFormat.Bgra8, global::Windows.Graphics.Imaging.BitmapAlphaMode.Premultiplied,
                (uint)bitmap.PixelWidth, (uint)bitmap.PixelHeight, 96, 96, pixels.ToArray());
            await encoder.FlushAsync();
            Check(bitmap.PixelWidth > 0 && bitmap.PixelHeight > 0, "native_winui_visual_tree_rendered_to_png");
        }
        catch (Exception exception) { error = exception.ToString(); }
        await File.WriteAllTextAsync(Field("report"), JsonSerializer.Serialize(new
        {
            schema_version = 1, status = error is null ? "pass" : "fail", platform = "native Windows x64",
            framework = "WinUI 3 / .NET 8", filesystem, created_image = create, table = create ? table : null,
            checks, error, source_unchanged = sourceUnchanged,
            limits = new[] { "Controller integration and actual XAML rendering; picker and input automation not exercised" }
        }, new JsonSerializerOptions { WriteIndented = true }));
        Close();
    }
}
#endif
