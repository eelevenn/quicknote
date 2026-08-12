using System.Runtime.InteropServices;

namespace QuickNote.StackBenchmark.WinUI;

internal static partial class NativeMethods
{
    internal const int HotkeyId = 0x514E;
    internal const uint WmHotkey = 0x0312;
    internal const uint WmClose = 0x0010;
    internal const uint WmTray = 0x8001;
    internal const uint WmLeftButtonDoubleClick = 0x0203;
    internal const uint WmRightButtonUp = 0x0205;
    internal const uint ModAlt = 0x0001;
    internal const uint ModControl = 0x0002;
    internal const uint VkQ = 0x51;

    internal delegate nint WindowProcedure(nint hwnd, uint message, nuint wParam, nint lParam);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool RegisterHotKey(nint windowHandle, int id, uint modifiers, uint virtualKey);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool UnregisterHotKey(nint windowHandle, int id);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    internal static extern nint SetWindowLongPtr(nint windowHandle, int index, nint newProcedure);

    [DllImport("user32.dll")]
    internal static extern nint CallWindowProc(nint previousProcedure, nint windowHandle, uint message, nuint wParam, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool ShowWindow(nint windowHandle, int command);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool SetForegroundWindow(nint windowHandle);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    internal static extern nint LoadIcon(nint instance, nint iconName);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool Shell_NotifyIcon(uint message, ref NotifyIconData data);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    internal struct NotifyIconData
    {
        internal uint size;
        internal nint windowHandle;
        internal uint id;
        internal uint flags;
        internal uint callbackMessage;
        internal nint icon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] internal string tip;
        internal uint state;
        internal uint stateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] internal string info;
        internal uint versionOrTimeout;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] internal string infoTitle;
        internal uint infoFlags;
        internal Guid guid;
        internal nint balloonIcon;
    }
}
