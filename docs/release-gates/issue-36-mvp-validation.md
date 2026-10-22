# Issue 36：v0.1 文字 MVP 验证记录

2026-10-22 在当前 Windows 开发机完成 [Issue 36](https://github.com/eelevenn/quicknote/issues/36) 的本地交付验证。产物为未签名的 MSI、便携 ZIP、相对路径 SHA-256 清单和构建清单；公开签名发行的独立门槛仍由 [Issue 22](issue-22-release-candidate.md) 跟踪。

## 已验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --locked --workspace` | 42 通过、0 失败；规模搜索与真实通知两项显式验收默认忽略 |
| Windows x64 Release | 通过；实际体积与摘要记录在交付清单中 |
| 动态运行时依赖 | 无动态 VCRUNTIME、MSVCP、CONCRT 导入 |
| MSI 静态检查 | 每用户安装、产品身份、升级/卸载表与体积门槛通过；签名状态为 `NotSigned` |
| 实际交付 EXE 的 UIA 旅程 | 以下 7 项全部通过，未使用真实便签数据 |

界面旅程覆盖：

1. 中文正文写入和真实自动保存。
2. 快速记录收起后后台常驻；Esc 由共享 Slint 键盘测试单独覆盖。
3. 单实例激活主页和完整正文搜索。
4. 系统关闭按钮隐藏主页并保持后台。
5. 从设置新建、归档和取消归档。
6. 系统保存对话框导出完整 JSON，以及立即备份。
7. 明确退出、损坏隔离、选择有效备份恢复，以及恢复后重新进入应用。

备份合同另覆盖当日快照不被后续编辑替换、备份失败仍保存正文、日/周轮换与 30 天保留期、恢复门禁跨重启保留、无有效备份时必须明确选择空库，以及永久清除同步移除旧快照。Windows 锁定备份时，永久清除会失败并保留回收站便签。

## 本轮定位与修复

- 系统文件对话框内的文件列表项与“保存”按钮可能共用 AutomationId `1`。验收脚本原先只按 ID 查找，会误选列表项；已同时限制 `ControlType.Button`，空库最小复现及完整旅程均通过。
- 数据库健康检查原先把所有 SQLite 检查错误都归为损坏。用独占锁复现后，已区分暂时锁定/I/O 错误和真实页损坏，避免不必要的隔离恢复。
- SQLite `total_changes` 包含已回滚的写操作。首次保存故障曾触发空库快照并占用当天备份；已结合事务成功结果判断，回归测试确认重试成功后快照包含实际正文。
- 校验清单保留 EXE 所在便携子目录的相对路径，支持从输出目录直接逐项校验。

## 复现与原始证据

```powershell
# 编译并打包当前源码。
.\scripts\New-QuickNoteMvp.ps1

# 只操作本次启动的进程与随机临时数据；检测到真实运行实例时会停止。
.\scripts\Test-QuickNoteMvp.ps1 `
  -ExecutablePath .\artifacts\mvp\QuickNote-0.1.0-x64\QuickNote.exe
```

本地原始证据保存在忽略目录：

- `artifacts/mvp/manifest.json`、`SHA256SUMS.txt`：实际交付文件、大小与摘要。
- `artifacts/mvp/runtime-dependencies-*.json`、`installer-validation-*.json`：依赖及 MSI 表检查结果。
- `artifacts/mvp-smoke/journeys.json`：整体成功标志、每项旅程、被测 EXE 的 SHA-256 和隔离数据位置。
- `artifacts/mvp-smoke/workspace-tests.txt`：工作区测试输出。

被测 EXE 的 SHA-256 与交付清单及 ZIP 内的 EXE 摘要逐项比对一致。主界面、窄窗口和设置页的软件渲染结果已检查；这不替代真实桌面上的 Narrator、缩放或高对比度验收。

## 未执行的独立矩阵

本轮没有执行签名、干净账户安装/升级/卸载、真实睡眠跨提醒、Explorer 重启、多显示器及人工无障碍矩阵，也没有重跑完整冷/热启动性能采样。它们继续保持未验证，不影响本次按维护者要求提供本地未签名 MVP。
