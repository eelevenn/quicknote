# Benchmark protocol v1

## 控制通道

每个候选监听 Windows named pipe `quicknote-stack-<candidate>`。消息和响应均为单行 UTF-8 JSON。

请求：

```json
{"id":"01","command":"status"}
{"id":"02","command":"hide"}
{"id":"03","command":"show"}
{"id":"04","command":"insert-sentinel","value":"§"}
{"id":"05","command":"shutdown"}
```

响应至少包含：

```json
{
  "id":"03",
  "ok":true,
  "candidate":"wpf",
  "pid":1234,
  "event":"editor-focused",
  "processStartTicks":123456,
  "hotkeyReceivedTicks":123999,
  "windowVisibleTicks":124050,
  "editorFocusedTicks":124080,
  "sentinelAcceptedTicks":124100
}
```

所有 `*Ticks` 均来自应用进程内的 `QueryPerformanceCounter`，同时返回 `frequency`。内部时间点仅用于诊断，正式延迟由外部 harness 的 `Stopwatch` 端到端测量。

## Readiness

`show` 和真实 `Ctrl+Alt+Q` 都必须完成以下步骤：

1. 显示窗口；
2. 请求前台激活；
3. 聚焦正文编辑器；
4. 插入再移除 sentinel character，证明编辑器可以修改；
5. 返回 `editor-focused` acknowledgement。

5 秒内未完成即记为 failure。原始失败样本不得删除。

## SQLite

所有候选使用相同的最小 schema：

```sql
CREATE TABLE IF NOT EXISTS notes (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    body TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

连接初始化后执行 `PRAGMA journal_mode=WAL;` 与 `PRAGMA synchronous=NORMAL;`。正文改变后进行 250 ms trailing debounce；隐藏窗口、退出及正常关闭前同步 flush。

## 采样

- warm-up：2 次，不计入统计。
- cold launch：20 次，每次结束完整进程树并等待 5 秒。
- hot summon：50 次，通过真实 `Ctrl+Alt+Q` 触发。
- idle memory：完成启动、编辑、保存、隐藏后稳定 30 秒，再以 1 Hz 采样 10 秒。
- percentile：保留全部样本，使用 nearest-rank 算法计算 P50/P95。
