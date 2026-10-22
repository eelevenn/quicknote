# Issue 36：v0.1 文字 MVP 验证记录

2026-10-22 在当前 Windows 开发机完成 [Issue 36](https://github.com/eelevenn/quicknote/issues/36) 的本地交付验证。产物为未签名的 MSI、便携 ZIP、相对路径 SHA-256 清单和构建清单；公开签名发行的独立门槛仍由 [Issue 22](issue-22-release-candidate.md) 跟踪。

## 已验证

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --locked --workspace` | 49 通过、0 失败；规模搜索与真实通知两项默认忽略，已分别显式运行通过 |
| 规模搜索 | 10,000 条便签、约 100 MiB 正文；20 次采样，P95 1.784 ms，门槛 200 ms |
| 真实 Windows 通知 | 实际投递、读取通知历史及通过生产取消接口移除历史均通过 |
| Windows x64 Release | 通过；实际体积与摘要记录在交付清单中 |
| 动态运行时依赖 | 无动态 VCRUNTIME、MSVCP、CONCRT 导入 |
| MSI 静态检查 | 每用户安装、产品身份、升级/卸载表与体积门槛通过；签名状态为 `NotSigned` |
| 实际交付 EXE 的 UIA 旅程 | 以下 7 项全部通过，未使用真实便签数据 |
| 冷启动 / 快捷键热唤起 | 20 / 50 次采样，P95 276.065 / 62.299 ms，门槛 900 / 90 ms |
| 保存正文后收起的后台内存 | 空闲 30 秒后 10 次采样，私有工作集 P95 5.36 MiB，门槛 60 MiB |
| MSI 管理员映像提取 | 提取 EXE 与被测程序摘要一致；产品安装注册状态未改变 |
| 依赖审计 | 598 个锁定包：已知安全漏洞 0、停止维护提示 7；595 个 registry 包无撤回版本或元数据校验和不匹配 |

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

最终复核另修复了永久清除遗留提醒正文、自动清理失败阻断打开健康数据库、备份失败状态跨重启丢失、部分成功操作未触发每日备份、导出合同不完整、提醒重试退避上限不符及已投递通知未清理。快速唤起改由事件驱动，并延迟到当前原生回调返回后执行，解决热唤起超标及 UIA 焦点重入崩溃。逐项证据和 Standards / Spec 两个审查轴见 [最终复核记录](v0.1-final-review.md)。

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
- `artifacts/final-review/mvp-ui-deferred/journeys.json`：最终 7 项旅程、被测 EXE 的 SHA-256 和隔离数据清理状态。
- `artifacts/final-review/workspace-tests.txt`、`search-scale.txt`、`windows-notification.txt`：工作区及独立验收输出。
- `artifacts/final-review/windows-shell-final/20261022-232738.json`：冷/热启动、内存样本及门槛。
- `artifacts/final-review/msi-payload.json`、`dependency-audit.md`：安装载荷与依赖审计证据。

被测 EXE 的 SHA-256 与交付清单及 ZIP 内的 EXE 摘要逐项比对一致。主界面、窄窗口和设置页的软件渲染结果已检查；这不替代真实桌面上的 Narrator、缩放或高对比度验收。

## 未执行的独立矩阵

本轮没有执行签名、干净账户安装/维修/升级/卸载、真实睡眠跨提醒、Explorer 重启、多显示器及人工无障碍矩阵。MSI 提取不能替代安装生命周期，UIA 和键盘测试也不能替代 Narrator、高对比度及缩放验收；这些项目继续保持未验证。当前机器已有用户数据及手工安装，本轮保持原状；本地未签名 MVP 的交付范围遵循维护者要求。
