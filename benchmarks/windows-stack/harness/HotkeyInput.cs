using System;
using System.Runtime.InteropServices;

// Send a real Ctrl+Alt+Q chord and return the QPC tick after the final key-up.
public static class BenchmarkHotkeyInput
{
    private const uint InputKeyboard = 1;
    private const ushort VkControl = 0x11;
    private const ushort VkMenu = 0x12;
    private const ushort VkQ = 0x51;
    private const uint KeyUp = 0x0002;

    [StructLayout(LayoutKind.Sequential)]
    private struct Input
    {
        public uint type;
        public InputUnion data;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)] public KeyboardInput keyboard;
        [FieldOffset(0)] public MouseInput mouse;
        [FieldOffset(0)] public HardwareInput hardware;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int dx;
        public int dy;
        public uint mouseData;
        public uint flags;
        public uint time;
        public UIntPtr extraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public ushort virtualKey;
        public ushort scanCode;
        public uint flags;
        public uint time;
        public UIntPtr extraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct HardwareInput
    {
        public uint message;
        public ushort parameterLow;
        public ushort parameterHigh;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint inputCount, Input[] inputs, int size);

    public static void Send()
    {
        var inputs = new[]
        {
            Key(VkControl, false), Key(VkMenu, false), Key(VkQ, false),
            Key(VkQ, true), Key(VkMenu, true), Key(VkControl, true)
        };
        var sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input)));
        if (sent != inputs.Length)
        {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
    }

    private static Input Key(ushort virtualKey, bool keyUp)
    {
        return new Input
        {
            type = InputKeyboard,
            data = new InputUnion
            {
                keyboard = new KeyboardInput
                {
                    virtualKey = virtualKey,
                    flags = keyUp ? KeyUp : 0
                }
            }
        };
    }
}
