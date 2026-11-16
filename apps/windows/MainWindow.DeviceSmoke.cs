#if XETCH_GUI_SMOKE
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace XEtch.Windows;

public sealed partial class MainWindow
{
    private static Button? FindDialogPrimary(DependencyObject root)
    {
        if (root is Button { Name: "PrimaryButton" } button) return button;
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
            if (FindDialogPrimary(VisualTreeHelper.GetChild(root, i)) is { } found) return found;
        return null;
    }

    private async Task SmokeDeviceAsync(JsonElement config)
    {
        string Field(string key) => config.GetProperty(key).GetString()!;
        var checks = new List<string>();
        string? error = null;
        void Check(bool condition, string name)
        {
            if (!condition) throw new InvalidOperationException(name);
            checks.Add(name);
        }
        try
        {
            Check(DevicesButton.IsEnabled, "read_only_devices_available");
            var inventory = await ReadDevicesAsync();
            Check(!inventory.GetProperty("write_enabled").GetBoolean(), "production_device_write_disabled");
            var browser = CreateDeviceDialog(inventory);
            var browserTask = browser.ShowAsync();
            await Task.Delay(200);
            Check(string.IsNullOrEmpty(browser.PrimaryButtonText), "read_only_browser_has_no_write_action");
            browser.Hide();
            await browserTask;
            Check(_workspace is null, "device_browser_does_not_change_image_session");
            Check(inventory.GetProperty("devices").EnumerateArray().Any(d => d.GetProperty("roles").EnumerateArray().Any(r => r.GetString() is "system" or "boot") && d.GetProperty("protection").GetProperty("hard_protected").GetBoolean()), "system_and_boot_protection_visible");
            bool denied = false;
            try { await Call("device.commit", new { force = true, confirmed = true }); }
            catch (BackendException) { denied = true; }
            Check(denied, "gui_cannot_bypass_production_guard");
            if (config.TryGetProperty("inventory_only", out var inventoryOnly) && inventoryOnly.GetBoolean())
            {
                await File.WriteAllTextAsync(Field("report"), JsonSerializer.Serialize(new { schema_version = 1, status = "pass", platform = "native Windows x64", checks, error = (string?)null, read_only = true }, new JsonSerializerOptions { WriteIndented = true }));
                Close();
                return;
            }
            await using var probe = new BackendClient(Field("probe"), new[] { "serve", "--root", Field("root") });
            Task<JsonElement> Status() => probe.CallAsync("validation.device.status", new { hash = true });
            var state = await Status();
            Check(state.GetProperty("exclusive").GetBoolean() && state.GetProperty("target_write_calls").GetInt32() == 0, "test_device_lease_held_before_preview");
            var plan = await probe.CallAsync("validation.device.plan", new { image = "planned.img" });
            var cancelled = new DeviceWriteConfirmation(Root.XamlRoot, plan);
            var shown = cancelled.ShowAsync();
            await Task.Delay(200);
            Check(cancelled.Dialog.DefaultButton == ContentDialogButton.Close, "confirmation_defaults_to_cancel");
            Check(!cancelled.Dialog.IsPrimaryButtonEnabled, "empty_phrase_disables_write");
            cancelled.Input.Text = "wrong";
            Check(!cancelled.Dialog.IsPrimaryButtonEnabled, "wrong_phrase_disables_write");
            cancelled.Dialog.Hide();
            Check(!await shown, "dismissal_does_not_authorize");
            await probe.CallAsync("validation.device.cancel", new { });
            Check((await Status()).GetProperty("target_write_calls").GetInt32() == 0, "cancelled_dialog_zero_writes");

            plan = await probe.CallAsync("validation.device.plan", new { image = "planned.img" });
            var shortPlan = JsonNode.Parse(plan.GetRawText())!;
            shortPlan["expires_in_ms"] = 200;
            using var shortDocument = JsonDocument.Parse(shortPlan.ToJsonString());
            var expired = new DeviceWriteConfirmation(Root.XamlRoot, shortDocument.RootElement);
            Check(!await expired.ShowAsync(), "dialog_timeout_cancels");
            await probe.CallAsync("validation.device.cancel", new { });
            Check((await Status()).GetProperty("target_write_calls").GetInt32() == 0, "timed_out_dialog_zero_writes");

            plan = await probe.CallAsync("validation.device.plan", new { image = "planned.img" });
            var accepted = new DeviceWriteConfirmation(Root.XamlRoot, plan);
            shown = accepted.ShowAsync();
            await Task.Delay(200);
            accepted.Input.Text = plan.GetProperty("phrase").GetString()!;
            await Task.Delay(100);
            Check(accepted.Dialog.IsPrimaryButtonEnabled, "exact_phrase_enables_write");
            var button = FindDialogPrimary(accepted.Dialog) ?? throw new InvalidOperationException("Primary dialog button not found.");
            var peer = FrameworkElementAutomationPeer.CreatePeerForElement(button) ?? new ButtonAutomationPeer(button);
            ((IInvokeProvider)peer.GetPattern(PatternInterface.Invoke)).Invoke();
            Check(await shown, "actual_primary_button_event_accepts_plan");
            var authorization = await probe.CallAsync("validation.device.confirm", new { plan_id = plan.GetProperty("plan_id").GetString(), phrase = accepted.Input.Text });
            var consent = new { plan_id = plan.GetProperty("plan_id").GetString(), authorization = authorization.GetProperty("authorization").GetString() };
            var committed = await probe.CallAsync("validation.device.commit", consent);
            Check(committed.GetProperty("committed").GetBoolean(), "confirmed_test_write_commits");
            Check((await Status()).GetProperty("partition_sha256").GetString() == Field("expected_sha256"), "gui_commit_full_partition_hash_matches");
            denied = false;
            try { await probe.CallAsync("validation.device.commit", consent); }
            catch (BackendException) { denied = true; }
            Check(denied, "gui_consent_is_single_use");
        }
        catch (Exception exception) { error = exception.ToString(); }
        await File.WriteAllTextAsync(Field("report"), JsonSerializer.Serialize(new { schema_version = 1, status = error is null ? "pass" : "fail", platform = "native Windows x64", test_only = true,
            checks, error, limits = new[] { "Actual WinUI dialog and automation-peer button event; physical keyboard/picker/accessibility not exercised.", "GUI timeout display uses a shortened copied preview; actual backend expiration is separately tested by the native controller." } }, new JsonSerializerOptions { WriteIndented = true }));
        Close();
    }
}
#endif
