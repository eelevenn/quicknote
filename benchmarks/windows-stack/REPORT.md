# Issue #6：候选技术栈性能与体积基准

> 结论边界：本报告记录一次 Windows 实机上的 exploratory benchmark。性能、体积与延迟均不设 hard gate；结果用于后续 Issue #7 的架构讨论，不在此处作技术栈决策。

## 摘要

四套候选都完成了同一最小工作负载，并在正式 run 中取得 0 failure。结果呈现明显取舍：Slint 的 cold launch 与 idle private working set 最低，WinUI 3 的 hot summon 略低，WPF 的 release directory 最小；Tauri 在本机使用现有 WebView2 runtime 时，hot summon 与 idle memory 均为四者最高。

| 候选 | Cold P50 / P95 | Hot P50 / P95 | Idle private WS P50 / P95 | Release directory | 失败 |
| --- | ---: | ---: | ---: | ---: | ---: |
| WPF + .NET Framework 4.8.1 | 673.1 / 882.4 ms | 48.0 / 64.4 ms | 39.87 / 39.87 MiB | **2.23 MiB** | 0 / 80 |
| Tauri 2 + Rust | 633.0 / 706.7 ms | 64.5 / 94.9 ms | 91.01 / 91.69 MiB | 4.23 MiB | 0 / 80 |
| Rust + Slint | **126.4 / 128.7 ms** | 32.4 / 33.6 ms | **5.78 / 5.87 MiB** | 10.22 MiB | 0 / 80 |
| WinUI 3 + C#/.NET | 474.0 / 506.2 ms | **32.3 / 33.5 ms** | 38.09 / 38.09 MiB | 78.65 MiB | 0 / 80 |

原始 bytes、未取整的 milliseconds 与 shared-runtime inventory 见 [`results/20260813-exploratory`](results/20260813-exploratory)。

## 等价工作负载

四套 Release prototype 均实现：

- single-instance 后台应用与 system tray；
- 真实 `Ctrl+Alt+Q` global hotkey；
- 显示、激活窗口并 focus 多行纯文本 editor；
- SQLite 单便签读写，`WAL`、`synchronous=NORMAL`、250 ms trailing debounce、hide/exit flush；
- 相同 `720 × 480` logical-pixel 窗口和约 8 KiB UTF-8 fixture；
- 相同 Windows named-pipe measurement contract。

协议、readiness 定义与 schema 见 [`contract/PROTOCOL.md`](contract/PROTOCOL.md)。四套实现只是 benchmark slice，不包含通知、主页、Markdown、语音或产品级 UI。

## 方法

- Build：各候选的 Release 配置；WinUI 3 为 `framework-dependent` publish。
- Warm-up：每个候选 2 次，不进入数据集。
- Cold launch：20 次；完整退出后等待 5 秒再启动。这是 process-cold/runtime-warm，不清除 Windows file cache。
- Hot summon：常驻进程隐藏后，通过 Windows `SendInput` 发送真实 `Ctrl+Alt+Q`，50 次。
- Readiness：窗口已显示、请求前台激活、editor 已 focus，并能插入再移除 sentinel character；5 秒超时记 failure。
- Idle memory：隐藏并等待 30 秒后，以 1 Hz 采样 10 次；数值为完整 descendant process tree 的 `WorkingSetPrivate` 总和。
- 统计：保留全部样本，用 nearest-rank 计算 P50/P95；不删除 outlier 或 failure。
- Release directory：构建输出目录内所有文件之和；不是压缩 installer 大小。

curated result directory 使用稳定名称 `20260813-exploratory`，由同机同环境的 serial runs `20260813-034539`（WPF、WinUI）、`20260813-040918`（Tauri）与 `20260813-041246`（Slint）合成。Rust 两项的最终 runs 加入了 Release GUI subsystem，消除 console host 这一错误的 desktop-app 混杂因素。每套 CSV 均为 80 行，10 条 idle-memory 记录的 `privateWorkingSetBytes`、`privateBytes`、`processCount` 均完整。

## 测试环境

| 项目 | 值 |
| --- | --- |
| OS | Windows 11 Pro 10.0.26200，build 26200 |
| CPU | AMD Ryzen 5 7500F，6 cores / 12 logical processors |
| RAM | 33,999,364,096 bytes（约 31.66 GiB） |
| GPU | NVIDIA GeForce RTX 4060 Ti；另有两个 virtual display adapters |
| Power scheme | Balanced |
| PowerShell | 5.1.26100.8972 |
| .NET SDK | 10.0.400 |
| Rust | rustc 1.97.1；cargo 1.97.1；MSVC toolchain |
| Node.js / npm | 24.19.0 / 11.17.0 |

完整机器快照见 [`environment.json`](results/20260813-exploratory/environment.json)。

## 体积与 shared runtime 边界

本轮为快速 exploratory run，未制作 installer，也未建立干净 VM，因此 installer size、clean-install download 与真实安装增量均记为 `not measured`，不能用 `release directory` 替代。

| 候选 | Release directory | 共享依赖边界 | 本机检测，仅作上下文 |
| --- | ---: | --- | --- |
| WPF | 2.23 MiB / 11 files | .NET Framework 4.8.1 | registry release `533509`；未单独计算系统组件体积 |
| Tauri | 4.23 MiB / 1 file | Evergreen WebView2 | 151.0.4129.78；runtime directory 约 849.59 MiB，属于共享浏览器 runtime，不是本 app 安装增量 |
| Slint | 10.22 MiB / 1 file | 无 WebView/.NET app runtime | SQLite 与 renderer 均进入 binary |
| WinUI 3 | 78.65 MiB / 51 files | .NET 10 与 Windows App Runtime 2 | 本机 .NETCore 10.0.11 约 76.13 MiB；Windows App Runtime 2.3.1 x64 约 107.79 MiB；这些是共享 runtime，不是 clean-install 差值 |

WinUI 3 publish directory 中约 62.23 MiB 来自 `Microsoft.Windows.SDK.NET.dll`、`onnxruntime.dll`、`DirectML.dll` 三个最大文件。它解释了本次 publish footprint，但不代表一个经过 trim、定制 packaging 或未来 SDK 优化后的下限。

## Correctness evidence

每个候选独立通过以下 lifecycle smoke，`failureCount=0`：

- second launch 自动退出，保留 single active instance；
- 10 次真实 hotkey → show/activate/focus/readiness；
- 3 次 sentinel mutation 后 hide lifecycle；
- 5 次 shutdown/restart recovery。

此外对 WPF 做了人工 UI smoke：editor 获得实际 focus；输入 `SMOKE-中文-粘贴-Undo-20260813`，再执行 `Ctrl+Z` / `Ctrl+Y`，文本分别正确撤销与重做。各候选机器可读结果见 `*-correctness.json`。

这些检查没有断言 SQLite 中的完整正文，也没有模拟 kill/power-loss，因此只证明交互与正常生命周期通路；它们不是 content-level crash-recovery、IME composition、Explorer restart 或 installer upgrade/uninstall 证明。

## 解释与限制

- 这是单台日常机器的三次顺序运行；未 round-robin，也未关闭其他前台/后台软件。
- 20 个 cold 和 50 个 hot 样本适合发现量级差异，不足以支持严格的 P95 SLA。
- Tauri memory 的 process tree 共 7 个 processes（主进程与 WebView2 children）；其他样本也按相同规则合计，因此结果反映部署形态而不是仅主进程。
- Slint Release binary 使用 Windows GUI subsystem，idle sample 的 process tree 只有 app process。
- 未测 energy、CPU utilization、GPU usage、IME、accessibility、签名、installer、更新、卸载与首次安装 runtime 下载。
- 本报告不把速度、内存或体积最优者直接视为整体技术栈最优者；维护成本、Windows integration、UI 能力与团队熟悉度需在后续决策中单独权衡。

## 复现

在 Windows PowerShell 5.1、仓库根目录执行：

```powershell
# 检查工具链与已有 candidate artifacts。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Doctor

# 以 Release 配置构建四套 prototype。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Build -Candidate all

# 逐候选执行 lifecycle correctness smoke。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Test-CandidateCorrectness.ps1 -Candidate wpf

# 执行 2 warm-up、20 cold、50 hot 与 10 idle samples。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Measure -Candidate all
```

Rust candidate 会先复制到 `%TEMP%\quicknote-stack-build` 的本机目录再构建，以避开 Windows MSVC/Rust 工具链对 WSL UNC working directory 的限制。
