using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;

namespace QuickNote.StackBenchmark.Wpf;

internal sealed class PipeServer : IDisposable
{
    private const string PipeName = "quicknote-stack-wpf";
    private readonly Func<string, string?, BenchmarkStatus> _commandHandler;
    private readonly CancellationTokenSource _cancellation = new();
    private Task? _listener;

    internal PipeServer(Func<string, string?, BenchmarkStatus> commandHandler)
    {
        _commandHandler = commandHandler;
    }

    internal void Start() => _listener = Task.Run(ListenLoopAsync);

    public void Dispose()
    {
        _cancellation.Cancel();
        try { _listener?.Wait(TimeSpan.FromSeconds(1)); } catch { /* Shutdown is best-effort. */ }
        _cancellation.Dispose();
    }

    private async Task ListenLoopAsync()
    {
        while (!_cancellation.IsCancellationRequested)
        {
            using var pipe = new NamedPipeServerStream(
                PipeName,
                PipeDirection.InOut,
                1,
                PipeTransmissionMode.Byte,
                PipeOptions.Asynchronous);

            try
            {
                await pipe.WaitForConnectionAsync(_cancellation.Token).ConfigureAwait(false);
                using var reader = new StreamReader(pipe, new UTF8Encoding(false), false, 1024, true);
                using var writer = new StreamWriter(pipe, new UTF8Encoding(false), 1024, true) { AutoFlush = true };
                var line = await reader.ReadLineAsync().ConfigureAwait(false);
                var id = ReadJsonString(line, "id");
                var command = ReadJsonString(line, "command") ?? "status";
                var value = ReadJsonString(line, "value");
                var status = _commandHandler(command, value);
                status.id = id;
                await writer.WriteLineAsync(status.ToJson()).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (_cancellation.IsCancellationRequested)
            {
                return;
            }
            catch (IOException)
            {
                // A polling client may disconnect between request and response.
            }
        }
    }

    // 协议输入由本仓库 harness 生成；小型解析器避免给 WPF 候选增加 JSON runtime。
    private static string? ReadJsonString(string? json, string property)
    {
        if (string.IsNullOrWhiteSpace(json))
        {
            return null;
        }

        var match = Regex.Match(
            json,
            "\\\"" + Regex.Escape(property) + "\\\"\\s*:\\s*\\\"(?<value>(?:\\\\.|[^\\\"])*)\\\"",
            RegexOptions.CultureInvariant);
        return match.Success ? Regex.Unescape(match.Groups["value"].Value) : null;
    }
}

// Public fields keep JavaScriptSerializer output stable on .NET Framework.
internal sealed class BenchmarkStatus
{
    public string? id;
    public bool ok = true;
    public string candidate = "wpf";
    public int pid = Process.GetCurrentProcess().Id;
    public string eventName = "status";
    public long frequency = Stopwatch.Frequency;
    public long processStartTicks;
    public long hotkeyReceivedTicks;
    public long windowVisibleTicks;
    public long editorFocusedTicks;
    public long sentinelAcceptedTicks;
    public long showSequence;
    public bool hotkeyRegistered;
    public string? error;

    internal string ToJson()
    {
        return "{" +
               $"\"id\":{JsonString(id)}," +
               $"\"ok\":{ok.ToString().ToLowerInvariant()}," +
               $"\"candidate\":{JsonString(candidate)}," +
               $"\"pid\":{pid}," +
               $"\"event\":{JsonString(eventName)}," +
               $"\"frequency\":{frequency}," +
               $"\"processStartTicks\":{processStartTicks}," +
               $"\"hotkeyReceivedTicks\":{hotkeyReceivedTicks}," +
               $"\"windowVisibleTicks\":{windowVisibleTicks}," +
               $"\"editorFocusedTicks\":{editorFocusedTicks}," +
               $"\"sentinelAcceptedTicks\":{sentinelAcceptedTicks}," +
               $"\"showSequence\":{showSequence}," +
               $"\"hotkeyRegistered\":{hotkeyRegistered.ToString().ToLowerInvariant()}," +
               $"\"error\":{JsonString(error)}" +
               "}";
    }

    private static string JsonString(string? value)
    {
        if (value is null)
        {
            return "null";
        }

        var escaped = value
            .Replace("\\", "\\\\")
            .Replace("\"", "\\\"")
            .Replace("\r", "\\r")
            .Replace("\n", "\\n");
        return $"\"{escaped}\"";
    }
}
