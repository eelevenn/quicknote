using System;
using System.IO;
using System.Threading;
using System.Windows;

namespace QuickNote.StackBenchmark.Wpf;

public partial class App : Application
{
    private const string MutexName = "Local\\QuickNote.StackBenchmark.Wpf";
    private Mutex? _singleInstanceMutex;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        _singleInstanceMutex = new Mutex(true, MutexName, out var ownsMutex);
        if (!ownsMutex)
        {
            Shutdown();
            return;
        }

        var dataDirectory = Environment.GetEnvironmentVariable("QUICKNOTE_BENCH_DATA_DIR");
        if (string.IsNullOrWhiteSpace(dataDirectory))
        {
            dataDirectory = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "QuickNoteStackBenchmark",
                "wpf");
        }

        var fixturePath = Environment.GetEnvironmentVariable("QUICKNOTE_BENCH_FIXTURE");
        var store = new NoteStore(dataDirectory, fixturePath);
        var window = new MainWindow(store);
        MainWindow = window;
        window.Show();
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _singleInstanceMutex?.Dispose();
        base.OnExit(e);
    }
}
