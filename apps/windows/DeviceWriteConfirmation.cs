using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using System.Diagnostics;
using System.Text.Json;

namespace XEtch.Windows;

/// <summary>D1 test-device dialog. Backend consent remains separate and mandatory.</summary>
internal sealed class DeviceWriteConfirmation
{
    internal ContentDialog Dialog { get; }
    internal TextBox Input { get; }
    private readonly string _phrase;
    private readonly TimeSpan _ttl;
    private bool _expired;

    internal DeviceWriteConfirmation(XamlRoot root, JsonElement plan)
    {
        if (!plan.GetProperty("test_only").GetBoolean())
            throw new InvalidOperationException("当前版本只允许验证新建的临时测试设备。");
        _phrase = plan.GetProperty("phrase").GetString() ?? throw new InvalidDataException("确认短语缺失。");
        if (string.IsNullOrWhiteSpace(_phrase)) throw new InvalidDataException("确认短语为空。");
        _ttl = TimeSpan.FromMilliseconds(Math.Clamp(plan.GetProperty("expires_in_ms").GetInt64(), 1, 120000));
        Input = new TextBox { Header = "输入上面的完整短语以确认", MaxLength = 160 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(Input, "DeviceWritePhrase");
        var panel = new StackPanel { Spacing = 12, MaxWidth = 620 };
        panel.Children.Add(new TextBlock { Text = "即将写入临时测试分区。开始后中断可能留下部分修改，需要使用恢复记录处理。", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(new TextBlock { Text = plan.GetProperty("target_label").GetString(), TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
        panel.Children.Add(new TextBlock { Text = $"目标标识：{plan.GetProperty("target_id").GetString()}\n分区偏移：{plan.GetProperty("partition_offset").GetUInt64()} 字节\n分区容量：{FileItem.FormatSize(plan.GetProperty("partition_size").GetUInt64())}\n修改：{plan.GetProperty("changed_blocks").GetInt32()} 块 / {FileItem.FormatSize(plan.GetProperty("changed_bytes").GetUInt64())}", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(new TextBlock { Text = $"恢复记录：{plan.GetProperty("recovery_directory").GetString()}", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(new TextBlock { Text = _phrase, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(Input);
        Dialog = new ContentDialog { XamlRoot = root, Title = "确认写入临时测试分区", Content = panel,
            PrimaryButtonText = "写入测试分区", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Close,
            IsPrimaryButtonEnabled = false };
        Input.TextChanging += (_, _) => Dialog.IsPrimaryButtonEnabled = !_expired && string.Equals(Input.Text, _phrase, StringComparison.Ordinal);
    }

    internal async Task<bool> ShowAsync()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        var watch = Stopwatch.StartNew();
        timer.Tick += (_, _) =>
        {
            if (watch.Elapsed >= _ttl)
            {
                _expired = true;
                Dialog.IsPrimaryButtonEnabled = false;
                Dialog.Hide();
            }
        };
        timer.Start();
        try { return await Dialog.ShowAsync() == ContentDialogResult.Primary && !_expired; }
        finally { timer.Stop(); }
    }
}
