using Microsoft.UI.Xaml;

namespace XEtch.Windows;

public partial class App : Application
{
    private Window? _window;
    public App() => InitializeComponent();
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        try
        {
            _window = new MainWindow();
            _window.Activate();
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"xEtch GUI startup failed: {error}");
            throw;
        }
    }
}
