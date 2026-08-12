# Windows 候选技术栈 exploratory benchmark

本目录为 Issue #6 提供四套等价的最小 Windows 原型和统一测量工具。它只建立可比较事实，不是 QuickNote 产品实现，也不替代 Issue #7 的架构决策。

## 固定工作负载

- 单实例后台应用、托盘图标和 `Ctrl+Alt+Q` 全局快捷键。
- 快捷键显示窗口、激活窗口并把焦点放入多行纯文本编辑器。
- SQLite 中只保存一张当前便签；启动时读取，编辑采用 250 ms trailing debounce，隐藏前强制保存。
- 四套窗口均使用 `720 × 480` logical pixels 和同一份 UTF-8 fixture。
- 不包含通知、主页、Markdown 渲染、语音和产品级视觉。

## 候选

| ID | 技术栈 | 默认部署口径 |
| --- | --- | --- |
| `wpf` | WPF + .NET Framework 4.8.1 | Windows 11 系统自带 framework |
| `tauri` | Tauri 2 + vanilla Web UI + Rust | 系统 Evergreen WebView2 |
| `slint` | Rust + Slint | 原生 binary，无 WebView/.NET app runtime |
| `winui` | WinUI 3 + C#/.NET | framework-dependent |

## 快速运行

在 Windows PowerShell 5.1 中执行。若本机 `ExecutionPolicy` 阻止仓库脚本，使用下面的进程级 `Bypass`；它不会修改系统策略：

```powershell
# 检查工具链和候选构建状态。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Doctor

# 构建全部候选；单个候选可用 -Candidate wpf。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Build -Candidate all

# 快速采样：2 次 warm-up、20 次冷启动、50 次热呼出。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Measure -Candidate all
```

正式结果写入 `results/<timestamp>/`。原始样本使用毫秒和 bytes；报告展示时使用 ms 与 MiB（`1 MiB = 1,048,576 bytes`）。

本机 exploratory 结果、shared runtime 边界与限制见 [`REPORT.md`](REPORT.md)。

## 快速模式的解释限制

- 原性能和体积目标仅作为 reference lines，不是 pass/fail hard gate。
- 20 个冷启动样本与 50 个热呼出样本只适合 exploratory comparison；特别是 P95 的置信度有限。
- 主冷启动指标是 process-cold/runtime-warm：完整进程树退出并等待 5 秒，但不清空 Windows file cache。
- 共享 runtime 的 clean-install 下载量和磁盘增量本轮不在 VM 中精确测量；只记录本机检测结果和官方分发边界。
- Rust 候选会自动复制到 `%TEMP%` 的本机目录构建，因为 MSVC/Rust/Tauri CLI 不能可靠地把 WSL UNC 路径作为工作目录。
- 缺失数据必须显示为 `null` 并附 blocker，不能当作候选表现差。

## 目录

```text
windows-stack/
├── contract/       # 所有候选共同遵守的 IPC 与工作负载协议
├── fixtures/       # 固定 SQLite seed 的源文本
├── harness/        # 构建、测量、汇总和环境快照脚本
├── results/        # 一次正式运行的原始 CSV/JSON（生成后提交）
├── slint/          # Rust + Slint 原型
├── tauri/          # Tauri 2 原型
├── winui/          # WinUI 3 原型
└── wpf/            # WPF 原型
```
