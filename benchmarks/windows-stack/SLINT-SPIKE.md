# Slint Windows 集成 spike

本 spike 回答 Issue #15 的架构风险问题，不是产品实现。结论是：带 Windows 原生集成后，Rust + Slint 仍满足已接受的性能与体积预算，并可把共享核心/UI 与桌面壳解耦。

## 架构边界

- `slint/src/core.rs`：平台中立的通知激活命令；URI 携带投递版本，保证同次动作幂等。通知正文负责打开，按钮负责“稍后提醒”；`Archive` 只保留为领域路由测试，不进入通知 UI。
- `slint/src/store.rs`：SQLite 便签、提醒事实源、迁移与补发收据；不引用 Win32 或 Windows Runtime。
- `slint/ui/app.slint`：共享主界面；显式包含 Slint 归属信息，以满足当前 Royalty-free License 的展示要求。
- `slint/src/platform.rs` 与 `slint/src/main.rs`：Windows 平台壳，负责计划通知、协议激活、热键、托盘、单实例与电源恢复。
- `desktop-windows` feature 只启用 Windows 依赖；`cargo check --lib --no-default-features` 已验证共享库不会拉入桌面壳。Android/iOS 正式构建仍留到移动端里程碑，iOS 不要求当前 CI 或产物。

## 可复现验证

在 Windows PowerShell 5.1 中执行：

```powershell
# 构建 Release 应用与真实每用户安装器。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Build -Candidate slint
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Build-SlintInstaller.ps1

# 验证安装、通知/激活、热键冲突、恢复和托盘重建消息。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Test-SlintIntegration.ps1

# 使用 STA 线程验证中文编辑、撤销/重做、UIA 名称和 150% 缩放。
powershell.exe -NoProfile -ExecutionPolicy Bypass -Sta -File .\benchmarks\windows-stack\harness\Test-SlintUi.ps1

# 采集带完整功能后的 20/50/10 性能样本。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\benchmarks\windows-stack\harness\Invoke-StackBenchmark.ps1 -Mode Measure -Candidate slint -ColdSamples 20 -HotSamples 50
```

## 结果

| 门槛 | 结果 | 预算 |
| --- | ---: | ---: |
| 冷启动 P95 | 167.07 ms | ≤ 900 ms |
| 热呼出 P95 | 43.60 ms | ≤ 90 ms |
| 空闲私有工作集 P95 | 6.86 MiB | ≤ 60 MiB |
| 每用户安装器 | 4.77 MiB | ≤ 18 MiB |
| 安装后文件 | 10.65 MiB | ≤ 45 MiB |

性能原始数据位于 `results/20260813-221106/`；Windows 集成与 UIA 证据位于 `results/20260813-slint-spike/`。全部 20 次冷启动、50 次热呼出与 10 次内存样本成功。

限制：自动化使用真实 `WM_POWERBROADCAST` 与 `TaskbarCreated` 消息走生产处理路径，但没有让当前工作站实际睡眠或强制终止 Explorer，以免打断用户会话。恢复处理只幂等标记“错过提醒”，不会补发 toast，符合 ADR-0001。测试会先清空 Windows 通知历史，再验证应用退出后的到点通知进入历史；通知动作通过已安装 exe 的协议冷启动/第二实例重定向完成。横幅是否可见仍受本机“请勿打扰”等系统策略控制。
