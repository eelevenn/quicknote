using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;

namespace QuickNote.StackBenchmark.WinUI;

internal sealed class BenchmarkWindow : Window
{
    private const int GwlWindowProcedure = -4;
    private const int SwHide = 0;
    private const int SwShow = 5;
    private readonly NoteStore _store;
    private readonly TextBox _editor;
    private readonly DispatcherTimer _saveTimer;
    private readonly PipeServer _pipe;
    private NativeMethods.NotifyIconData _trayIcon;
    private bool _trayIconAdded;
    private readonly long _processStartTicks = Stopwatch.GetTimestamp();
    private readonly NativeMethods.WindowProcedure _windowProcedure;
    private nint _previousWindowProcedure;
    private nint _windowHandle;
    private bool _loading = true;
    private bool _allowExit;
    private bool _hotkeyRegistered;
    private string? _lastError;
    private long _hotkeyReceivedTicks;
    private long _windowVisibleTicks;
    private long _editorFocusedTicks;
    private long _sentinelAcceptedTicks;
    private long _showSequence;

    internal BenchmarkWindow(NoteStore store)
    {
        _store = store;
        Title = "QuickNote · WinUI 3 benchmark";
        _editor = new TextBox
        {
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            FontSize = 15,
            Padding = new Thickness(14),
        };
        ScrollViewer.SetVerticalScrollBarVisibility(_editor, ScrollBarVisibility.Auto);
        _editor.TextChanged += (_, _) => ScheduleSave();

        var layout = new Grid { Padding = new Thickness(24), RowSpacing = 12 };
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.Children.Add(new TextBlock { Text = "当前便签", FontSize = 20 });
        Grid.SetRow(_editor, 1);
        layout.Children.Add(_editor);
        var footer = new TextBlock { Text = "Ctrl+Alt+Q 呼出 · 250 ms 自动保存 · benchmark prototype" };
        Grid.SetRow(footer, 2);
        layout.Children.Add(footer);
        Content = layout;

        _saveTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(250) };
        _saveTimer.Tick += (_, _) => Flush();
        _windowProcedure = WindowProcedure;
        Activated += OnActivated;
        Closed += OnClosed;
        _pipe = new PipeServer(HandlePipeCommand);
        _pipe.Start();
    }

    private void OnActivated(object sender, WindowActivatedEventArgs args)
    {
        if (_windowHandle != 0) return;
        _windowHandle = WinRT.Interop.WindowNative.GetWindowHandle(this);
        var windowId = Win32Interop.GetWindowIdFromWindow(_windowHandle);
        AppWindow.GetFromWindowId(windowId).Resize(new Windows.Graphics.SizeInt32(720, 480));
        _previousWindowProcedure = NativeMethods.SetWindowLongPtr(_windowHandle, GwlWindowProcedure, Marshal.GetFunctionPointerForDelegate(_windowProcedure));
        _hotkeyRegistered = NativeMethods.RegisterHotKey(_windowHandle, NativeMethods.HotkeyId, NativeMethods.ModControl | NativeMethods.ModAlt, NativeMethods.VkQ);
        if (!_hotkeyRegistered) _lastError = $"RegisterHotKey failed: {Marshal.GetLastWin32Error()}.";
        CreateTrayIcon();
        _editor.Text = _store.Load();
        _loading = false;
        ShowEditor(false);
    }

    private nint WindowProcedure(nint hwnd, uint message, nuint wParam, nint lParam)
    {
        if (message == NativeMethods.WmHotkey && (int)wParam == NativeMethods.HotkeyId)
        {
            _hotkeyReceivedTicks = Stopwatch.GetTimestamp();
            ShowEditor(true);
            return 0;
        }
        if (message == NativeMethods.WmClose && !_allowExit)
        {
            HideEditor();
            return 0;
        }
        if (message == NativeMethods.WmTray)
        {
            var mouseMessage = unchecked((uint)lParam);
            if (mouseMessage == NativeMethods.WmLeftButtonDoubleClick) ShowEditor(false);
            if (mouseMessage == NativeMethods.WmRightButtonUp) ShutdownApplication();
            return 0;
        }
        return NativeMethods.CallWindowProc(_previousWindowProcedure, _windowHandle, message, wParam, lParam);
    }

    private void ShowEditor(bool fromHotkey)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            NativeMethods.ShowWindow(_windowHandle, SwShow);
            _windowVisibleTicks = Stopwatch.GetTimestamp();
            NativeMethods.SetForegroundWindow(_windowHandle);
            _editor.Focus(FocusState.Programmatic);
            _editorFocusedTicks = Stopwatch.GetTimestamp();
            var selection = _editor.SelectionStart;
            _editor.Text = _editor.Text.Insert(selection, "§").Remove(selection, 1);
            _editor.SelectionStart = selection;
            _sentinelAcceptedTicks = Stopwatch.GetTimestamp();
            _showSequence++;
            if (!fromHotkey) _hotkeyReceivedTicks = 0;
        });
    }

    private void HideEditor()
    {
        DispatcherQueue.TryEnqueue(() => { Flush(); NativeMethods.ShowWindow(_windowHandle, SwHide); });
    }

    private void ScheduleSave()
    {
        if (_loading) return;
        _saveTimer.Stop();
        _saveTimer.Start();
    }

    private void Flush()
    {
        _saveTimer.Stop();
        _store.Save(_editor.Text);
    }

    private void CreateTrayIcon()
    {
        const uint add = 0;
        const uint messageFlag = 1;
        const uint iconFlag = 2;
        const uint tipFlag = 4;
        _trayIcon = new NativeMethods.NotifyIconData
        {
            size = (uint)Marshal.SizeOf<NativeMethods.NotifyIconData>(),
            windowHandle = _windowHandle,
            id = 1,
            flags = messageFlag | iconFlag | tipFlag,
            callbackMessage = NativeMethods.WmTray,
            icon = NativeMethods.LoadIcon(0, 32512),
            tip = "QuickNote WinUI benchmark",
            info = string.Empty,
            infoTitle = string.Empty
        };
        _trayIconAdded = NativeMethods.Shell_NotifyIcon(add, ref _trayIcon);
    }

    private BenchmarkStatus HandlePipeCommand(string command, string? value)
    {
        switch (command)
        {
            case "show": ShowEditor(false); break;
            case "hide": HideEditor(); break;
            case "insert-sentinel": ShowEditor(false); break;
            case "shutdown": DispatcherQueue.TryEnqueue(ShutdownApplication); break;
            case "status": break;
            default: return CreateStatus("error", $"Unknown command: {command}");
        }
        return CreateStatus(command == "status" ? "status" : "editor-focused", null);
    }

    private BenchmarkStatus CreateStatus(string eventName, string? error) => new()
    {
        ok = error is null && _lastError is null,
        @event = eventName,
        processStartTicks = _processStartTicks,
        hotkeyReceivedTicks = _hotkeyReceivedTicks,
        windowVisibleTicks = _windowVisibleTicks,
        editorFocusedTicks = _editorFocusedTicks,
        sentinelAcceptedTicks = _sentinelAcceptedTicks,
        showSequence = _showSequence,
        hotkeyRegistered = _hotkeyRegistered,
        error = error ?? _lastError
    };

    private void ShutdownApplication()
    {
        _allowExit = true;
        Flush();
        Close();
    }

    private void OnClosed(object sender, WindowEventArgs args)
    {
        Flush();
        if (_hotkeyRegistered) NativeMethods.UnregisterHotKey(_windowHandle, NativeMethods.HotkeyId);
        if (_trayIconAdded)
        {
            const uint delete = 2;
            NativeMethods.Shell_NotifyIcon(delete, ref _trayIcon);
        }
        _pipe.Dispose();
        _store.Dispose();
    }
}
