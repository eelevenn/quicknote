using Microsoft.UI.Xaml;

namespace QuickNote.StackBenchmark.WinUI;

public sealed partial class App : Application
{
    private readonly Mutex _singleInstanceMutex;
    private BenchmarkWindow? _window;

    public App()
    {
        _singleInstanceMutex = new Mutex(true, "Local\\QuickNote.StackBenchmark.WinUI", out var ownsMutex);
        if (!ownsMutex)
        {
            Exit();
            return;
        }
        InitializeComponent();
        UnhandledException += (_, args) => args.Handled = false;
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var dataDirectory = Environment.GetEnvironmentVariable("QUICKNOTE_BENCH_DATA_DIR")
            ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "QuickNoteStackBenchmark", "winui");
        _window = new BenchmarkWindow(new NoteStore(dataDirectory, Environment.GetEnvironmentVariable("QUICKNOTE_BENCH_FIXTURE")));
        _window.Activate();
    }
}
