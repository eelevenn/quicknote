# Issue 21：本地转写发布门槛

> **状态：已由 [ADR-0004](../adr/0004-mvp-excludes-voice-input.md) 取代。** 本文仅保留为历史实现和验证证据；语音不属于 v0.1.0 MVP，以下未完成人工项目不再阻断 v0.1.0 发布。

本文记录可自动复现的实现证据，以及仍必须由人工在目标 Windows 机器上完成的发布门槛。未勾选项目不得被解释为已通过。

## 固定资产

| 资产 | 版本 | 下载字节 | 解压字节 | SHA-256 | 许可证状态 |
| --- | --- | ---: | ---: | --- | --- |
| sherpa-onnx Windows x64 shared MT no-TTS | v1.13.5 | 22,925,630 | 62,645,394 | `7c9dbcd3d38f71e2ee25dafc270e91d30f0684be8526c3c19cac1aedb073033d` | Apache-2.0；第三方 notices 待人工复核 |
| SenseVoice-Small INT8 | 2024-07-17 | 163,002,883 | 240,506,435 | `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e` | FunASR Model Open Source License 1.1；转换物商业再分发链待人工复核 |

应用内清单位于 `crates/quicknote-app/assets/transcription-package.json`。运行时下载总量为 185,928,513 字节，解压总量为 303,151,829 字节，不进入主安装器或应用本体。

离线复验可只对当前应用进程设置 `QUICKNOTE_TRANSCRIPTION_ASSET_CACHE`，指向包含清单原始文件名的缓存目录。缓存仅替代网络传输，后续字节数、SHA-256、解压边界、完整 inventory 和自检不会跳过。

## 已自动验证

- `cargo test -p quicknote-app --lib transcription::`：15 项通过，覆盖固定清单、取消不切换、损坏检测与显式删除、自检失败保留旧 generation、静音/键盘瞬态门禁、临时音频清理、确定性错误不重试和瞬态错误仅重试一次。
- `cargo check -p quicknote-windows --target x86_64-pc-windows-msvc`：通过。
- `scripts/Build-TranscriptionSidecar.ps1`：固定 sherpa-onnx 资产大小和 SHA-256 校验通过，MSVC C++17 编译与链接通过。
- `scripts/build-windows.ps1`：完整 Release 构建通过；应用为 13,033,984 字节，sidecar 为 289,792 字节，二者均不包含按需模型或运行时。应用本体低于 45 MiB，但安装器体积仍须在 Issue 22 产物上复核。
- 使用固定模型自带 `zh.wav` 完成一次真实 sidecar 协议自检：返回 `ready` 与 `completed`，文字为“开饭时间早上9点至下午5点。”。本次开发机数据为冷加载 2,425.553 ms、推理 172.830 ms，仅证明协议和模型可运行，不计入目标机性能门槛。

## 2026-08-16 本机原生 UI 验收

- 在 Windows 25H2 x64 build 26200.9168、Ryzen 5 7500F（12 个逻辑处理器）、约 31.7 GiB 内存上安装 Release 应用和 sidecar；安装位置为 `%LOCALAPPDATA%\Programs\QuickNote`，并创建当前用户的开始菜单快捷方式。清除验收进程环境后普通冷启动成功，语音按钮仍可用并显示“本地语音输入待命”。
- 复用已缓存的同一组固定资产执行安装，仍经过清单字节数、SHA-256、完整 inventory、自检和原子切换；`current.json` 指向 `sensevoice-2024-07-17-sherpa-1.13.5`，`selfTest` 为 `passed`。首次网络下载取消后 staging 保持为空。
- 使用默认扬声器播放固定中文样本，并由默认真实麦克风回录；UI 得到可编辑预览“开放时间早上9点至下午5点，。”，确认后成功插入录音结束时冻结的光标并自动保存。隔离验收便签随后永久清除，原有两条便签未修改。
- 纯静音录音返回 `NoSpeech`，没有调用模型；成功、静音和放弃预览后 `operations` 目录均为空，没有残留临时 WAV 或 sidecar 进程。
- 转写结束后的应用 Working Set 为 48,664,576 字节，sidecar 已退出，应用进程没有活动 TCP 连接；三次 `audio_egress` 审计记录均为 0 字节。此快照不替代 Peak Commit、线程数和外部抓包门槛。
- 当前验收者已明确接受 79% 人工一次可用率风险；产品预览中的风险提示保留，未因接受风险而降低其他发布门槛。

## 待人工完成

- [ ] 完成 SenseVoice 转换物商业再分发链、sherpa-onnx 和全部第三方 notices 的人工许可复核。
- [ ] 在原生 Windows 11 25H2 x64、4 个可用逻辑核心、8 GiB、CPU-only 环境重跑固定基准。
- [ ] 证明热转写 P95：5 秒不超过 800 ms、30 秒不超过 2 s、60 秒不超过 4 s；同时记录冷加载、Peak Working Set、Peak Commit、CPU 和线程数。
- [ ] 证明 sidecar 退出后完整后代进程树回落到后台 60 MiB 硬预算；主安装器不超过 18 MiB，应用本体不超过 45 MiB。
- [ ] 在真实 UI 完成录音期间编辑响应、静音/键盘声/非语音、60 秒自动停止、冻结光标、可编辑预览、包下载取消和损坏修复的验收。
- [ ] 注入 sidecar 崩溃与超时，证明同一临时 WAV 最多重试一次；注入模型缺失、损坏和无语音，证明不重试。
- [ ] 覆盖成功、取消、失败、重试放弃和应用退出，证明临时 WAV 清理；再验证启动时只清理可取得独占锁的遗留目录。
- [ ] 使用独立网络观测工具分别捕获包下载窗口和纯转写窗口，证明纯转写期间没有音频或其他应用载荷外发。应用内 `network-audit.jsonl` 只作为操作分类佐证，不能替代外部抓包。

## 发布结论

SenseVoice 的人工一次可用率理论上限为 79%，低于原 90% 门槛。该失败曾要求重新打开 Issue 10 正式缩减范围；ADR-0004 已完成范围收缩，因此本段只保留为历史退出依据，不再约束 v0.1.0 文字-only 发布。
