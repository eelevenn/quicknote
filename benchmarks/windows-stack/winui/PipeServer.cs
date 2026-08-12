using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;

namespace QuickNote.StackBenchmark.WinUI;

internal sealed class PipeServer : IDisposable
{
    private readonly Func<string, string?, BenchmarkStatus> _handler;
    private readonly CancellationTokenSource _cancellation = new();
    private Task? _listener;

    internal PipeServer(Func<string, string?, BenchmarkStatus> handler) => _handler = handler;

    internal void Start() => _listener = Task.Run(ListenLoopAsync);

    public void Dispose()
    {
        _cancellation.Cancel();
        _cancellation.Dispose();
    }

    private async Task ListenLoopAsync()
    {
        while (!_cancellation.IsCancellationRequested)
        {
            await using var pipe = new NamedPipeServerStream("quicknote-stack-winui", PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
            try
            {
                await pipe.WaitForConnectionAsync(_cancellation.Token);
                using var reader = new StreamReader(pipe, new UTF8Encoding(false), leaveOpen: true);
                await using var writer = new StreamWriter(pipe, new UTF8Encoding(false), leaveOpen: true) { AutoFlush = true };
                var line = await reader.ReadLineAsync(_cancellation.Token);
                var request = JsonSerializer.Deserialize<PipeRequest>(line ?? "{}") ?? new PipeRequest();
                var status = _handler(request.command ?? "status", request.value);
                status.id = request.id;
                await writer.WriteLineAsync(JsonSerializer.Serialize(status));
            }
            catch (OperationCanceledException) when (_cancellation.IsCancellationRequested) { return; }
            catch (IOException) { /* Polling clients may disconnect during shutdown. */ }
        }
    }

    private sealed class PipeRequest
    {
        public string? id { get; set; }
        public string? command { get; set; }
        public string? value { get; set; }
    }
}

internal sealed class BenchmarkStatus
{
    public string? id { get; set; }
    public bool ok { get; set; } = true;
    public string candidate { get; set; } = "winui";
    public int pid { get; set; } = Environment.ProcessId;
    public string @event { get; set; } = "status";
    public long frequency { get; set; } = Stopwatch.Frequency;
    public long processStartTicks { get; set; }
    public long hotkeyReceivedTicks { get; set; }
    public long windowVisibleTicks { get; set; }
    public long editorFocusedTicks { get; set; }
    public long sentinelAcceptedTicks { get; set; }
    public long showSequence { get; set; }
    public bool hotkeyRegistered { get; set; }
    public string? error { get; set; }
}
