using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Drawing;
using System.Windows;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Threading;
using Forms = System.Windows.Forms;

namespace QuickNote.StackBenchmark.Wpf;

public partial class MainWindow : Window
{
    private readonly NoteStore _store;
    private readonly DispatcherTimer _saveTimer;
    private readonly PipeServer _pipeServer;
    private readonly Forms.NotifyIcon _trayIcon;
    private readonly long _processStartTicks = Stopwatch.GetTimestamp();
    private IntPtr _windowHandle;
    private bool _allowExit;
    private bool _loading = true;
    private bool _hotkeyRegistered;
    private string? _lastError;
    private long _hotkeyReceivedTicks;
    private long _windowVisibleTicks;
    private long _editorFocusedTicks;
    private long _sentinelAcceptedTicks;
    private long _showSequence;

    internal MainWindow(NoteStore store)
    {
        InitializeComponent();
        _store = store;
        _saveTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(250) };
        _saveTimer.Tick += (_, _) => Flush();

        _trayIcon = new Forms.NotifyIcon
        {
            Icon = SystemIcons.Application,
            Text = "QuickNote WPF benchmark",
            Visible = true,
            ContextMenuStrip = BuildTrayMenu()
        };
        _trayIcon.DoubleClick += (_, _) => ShowEditor(false);

        SourceInitialized += OnSourceInitialized;
        Loaded += OnLoaded;
        Editor.TextChanged += (_, _) => ScheduleSave();
        _pipeServer = new PipeServer(HandlePipeCommand);
        _pipeServer.Start();
    }

    private void OnSourceInitialized(object? sender, EventArgs e)
    {
        _windowHandle = new WindowInteropHelper(this).Handle;
        HwndSource.FromHwnd(_windowHandle)?.AddHook(WindowProcedure);
        _hotkeyRegistered = NativeMethods.RegisterHotKey(
            _windowHandle,
            NativeMethods.HotkeyId,
            NativeMethods.ModControl | NativeMethods.ModAlt,
            NativeMethods.VkQ);
        if (!_hotkeyRegistered)
        {
            _lastError = $"RegisterHotKey failed with Win32 error {System.Runtime.InteropServices.Marshal.GetLastWin32Error()}.";
        }
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        Editor.Text = _store.Load();
        _loading = false;
        ShowEditor(false);
    }

    private IntPtr WindowProcedure(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message == NativeMethods.WmHotkey && wParam.ToInt32() == NativeMethods.HotkeyId)
        {
            _hotkeyReceivedTicks = Stopwatch.GetTimestamp();
            ShowEditor(true);
            handled = true;
        }

        return IntPtr.Zero;
    }

    private Forms.ContextMenuStrip BuildTrayMenu()
    {
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("显示", null, (_, _) => ShowEditor(false));
        menu.Items.Add("退出", null, (_, _) => ShutdownApplication());
        return menu;
    }

    private void ShowEditor(bool fromHotkey)
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.Invoke(() => ShowEditor(fromHotkey));
            return;
        }

        if (WindowState == WindowState.Minimized)
        {
            WindowState = WindowState.Normal;
        }
        Show();
        _windowVisibleTicks = Stopwatch.GetTimestamp();
        Activate();
        Topmost = true;
        Topmost = false;
        Editor.Focus();
        Keyboard.Focus(Editor);
        _editorFocusedTicks = Stopwatch.GetTimestamp();

        // 插入再移除 sentinel，验证编辑器处于可修改状态且不污染正文。
        var caret = Editor.CaretIndex;
        Editor.Text = Editor.Text.Insert(caret, "§");
        Editor.Text = Editor.Text.Remove(caret, 1);
        Editor.CaretIndex = caret;
        _sentinelAcceptedTicks = Stopwatch.GetTimestamp();
        _showSequence++;

        if (!fromHotkey)
        {
            _hotkeyReceivedTicks = 0;
        }
    }

    private void HideEditor()
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.Invoke(HideEditor);
            return;
        }
        Flush();
        Hide();
    }

    private void ScheduleSave()
    {
        if (_loading)
        {
            return;
        }
        _saveTimer.Stop();
        _saveTimer.Start();
    }

    private void Flush()
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.Invoke(Flush);
            return;
        }
        _saveTimer.Stop();
        _store.Save(Editor.Text);
    }

    private BenchmarkStatus HandlePipeCommand(string command, string? value)
    {
        try
        {
            switch (command)
            {
                case "show":
                    Dispatcher.Invoke(() => ShowEditor(false));
                    break;
                case "hide":
                    Dispatcher.Invoke(HideEditor);
                    break;
                case "insert-sentinel":
                    Dispatcher.Invoke(() =>
                    {
                        var marker = string.IsNullOrEmpty(value) ? "§" : value!;
                        var caret = Editor.CaretIndex;
                        Editor.Text = Editor.Text.Insert(caret, marker);
                        Editor.Text = Editor.Text.Remove(caret, marker.Length);
                        Editor.CaretIndex = caret;
                        _sentinelAcceptedTicks = Stopwatch.GetTimestamp();
                    });
                    break;
                case "shutdown":
                    Dispatcher.BeginInvoke(new Action(ShutdownApplication));
                    break;
                case "status":
                    break;
                default:
                    return CreateStatus("error", $"Unknown command: {command}");
            }
            return CreateStatus(command == "status" ? "status" : "editor-focused", null);
        }
        catch (Exception exception)
        {
            return CreateStatus("error", exception.Message);
        }
    }

    private BenchmarkStatus CreateStatus(string eventName, string? error)
    {
        return new BenchmarkStatus
        {
            ok = error is null && _lastError is null,
            eventName = eventName,
            processStartTicks = _processStartTicks,
            hotkeyReceivedTicks = _hotkeyReceivedTicks,
            windowVisibleTicks = _windowVisibleTicks,
            editorFocusedTicks = _editorFocusedTicks,
            sentinelAcceptedTicks = _sentinelAcceptedTicks,
            showSequence = _showSequence,
            hotkeyRegistered = _hotkeyRegistered,
            error = error ?? _lastError
        };
    }

    private void ShutdownApplication()
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.Invoke(ShutdownApplication);
            return;
        }
        _allowExit = true;
        Flush();
        Close();
        Application.Current.Shutdown();
    }

    protected override void OnClosing(CancelEventArgs e)
    {
        if (!_allowExit)
        {
            e.Cancel = true;
            HideEditor();
            return;
        }

        if (_hotkeyRegistered && _windowHandle != IntPtr.Zero)
        {
            NativeMethods.UnregisterHotKey(_windowHandle, NativeMethods.HotkeyId);
        }
        _pipeServer.Dispose();
        _trayIcon.Visible = false;
        _trayIcon.Dispose();
        _store.Dispose();
        base.OnClosing(e);
    }
}
