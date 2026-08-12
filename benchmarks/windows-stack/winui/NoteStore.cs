using System.Text;
using Microsoft.Data.Sqlite;

namespace QuickNote.StackBenchmark.WinUI;

internal sealed class NoteStore : IDisposable
{
    private readonly SqliteConnection _connection;

    internal NoteStore(string dataDirectory, string? fixturePath)
    {
        Directory.CreateDirectory(dataDirectory);
        _connection = new SqliteConnection($"Data Source={Path.Combine(dataDirectory, "quicknote.db")};Mode=ReadWriteCreate");
        _connection.Open();
        Execute("PRAGMA journal_mode=WAL;");
        Execute("PRAGMA synchronous=NORMAL;");
        Execute("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY CHECK (id = 1), body TEXT NOT NULL, updated_at TEXT NOT NULL);");
        using var count = _connection.CreateCommand();
        count.CommandText = "SELECT COUNT(*) FROM notes WHERE id = 1;";
        if (Convert.ToInt64(count.ExecuteScalar()) == 0) Save(BuildSeed(fixturePath));
    }

    internal string Load()
    {
        using var command = _connection.CreateCommand();
        command.CommandText = "SELECT body FROM notes WHERE id = 1;";
        return Convert.ToString(command.ExecuteScalar()) ?? string.Empty;
    }

    internal void Save(string body)
    {
        using var transaction = _connection.BeginTransaction();
        using var command = _connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = "INSERT INTO notes (id, body, updated_at) VALUES (1, $body, $updated) ON CONFLICT(id) DO UPDATE SET body=excluded.body, updated_at=excluded.updated_at;";
        command.Parameters.AddWithValue("$body", body);
        command.Parameters.AddWithValue("$updated", DateTimeOffset.UtcNow.ToString("O"));
        command.ExecuteNonQuery();
        transaction.Commit();
    }

    public void Dispose() => _connection.Dispose();

    private void Execute(string sql)
    {
        using var command = _connection.CreateCommand();
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }

    private static string BuildSeed(string? fixturePath)
    {
        var source = fixturePath is not null && File.Exists(fixturePath)
            ? File.ReadAllText(fixturePath, Encoding.UTF8)
            : "QuickNote benchmark fixture · 中文输入 · SQLite autosave\n";
        var builder = new StringBuilder(source);
        while (Encoding.UTF8.GetByteCount(builder.ToString()) < 8 * 1024) builder.AppendLine().Append(source);
        return builder.ToString();
    }
}
