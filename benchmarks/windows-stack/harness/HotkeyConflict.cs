using System;
using System.Runtime.InteropServices;
using System.Threading;

// Own Ctrl+Shift+Q until this helper is terminated so conflict handling is reproducible.
public static class HotkeyConflict
{
    private const int HotkeyId = 0x5150;
    private const uint ModControl = 0x0002;
    private const uint ModShift = 0x0004;
    private const uint ModNoRepeat = 0x4000;
    private const uint VkQ = 0x51;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool RegisterHotKey(IntPtr window, int id, uint modifiers, uint virtualKey);

    [DllImport("user32.dll")]
    private static extern bool UnregisterHotKey(IntPtr window, int id);

    public static int Main()
    {
        if (!RegisterHotKey(IntPtr.Zero, HotkeyId, ModControl | ModShift | ModNoRepeat, VkQ))
        {
            return Marshal.GetLastWin32Error();
        }
        Console.WriteLine("READY");
        try
        {
            Thread.Sleep(Timeout.Infinite);
        }
        finally
        {
            UnregisterHotKey(IntPtr.Zero, HotkeyId);
        }
        return 0;
    }
}
