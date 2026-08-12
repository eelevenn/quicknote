# Windows 中文短语音转写方案研究

> 研究票据：[评估本地与云端语音转写方案](https://github.com/eelevenn/quicknote/issues/4)
>
> 调研日期：2026-08-12
>
> 结论性质：为 MVP 提供短名单与验证门槛，不在本报告中作最终选型。

## 结论摘要

QuickNote 的语音输入应做成**按需能力**，与应用本体、常驻进程和安装包完全解耦：默认只安装录音与适配层；用户首次选择“本地转写”时再下载模型及原生运行时；不用语音时不启动推理进程、不加载模型。这样本地模型不会破坏应用本体 `≤ 15 MB` 安装包、`≤ 50 MB` 后台空闲内存等目标。

MVP 保留以下短名单：

1. **本地优先候选：SenseVoice-Small INT8 + sherpa-onnx。** [官方模型卡](https://huggingface.co/FunAudioLLM/SenseVoiceSmall)在 AISHELL-1/2、WenetSpeech 等公开基准上报告其中文、粤语识别优于对比的 Whisper，并称非自回归架构较 Whisper-Small 快 5 倍以上；[sherpa-onnx 官方文档](https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html)提供 Windows x64、C/C++ API、麦克风/VAD 示例。代价是模型约 228 MB，[官方压缩发布包](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models)约 163 MB，而且缺少 Windows x64 峰值内存数据，必须原型实测后才能进入 MVP。
2. **本地小体积对照：whisper.cpp `base-q5_1`。** [官方模型仓库](https://huggingface.co/ggerganov/whisper.cpp)列出的多语言量化模型为 57 MiB；[运行时](https://github.com/ggml-org/whisper.cpp/blob/master/README.md)是无外部依赖的 C/C++ 实现并支持 Windows、CPU-only。但官方未给出该量化模型的内存与中文短句准确率，原始 `base` 的参考内存约 388 MB。它适合作为体积更小、许可证边界更清楚的对照方案，而不是未经实测直接认定为默认。
3. **可选云候选：OpenAI Transcription API。** [`gpt-4o-mini-transcribe` 官方模型页](https://developers.openai.com/api/docs/models/gpt-4o-mini-transcribe)称相较原始 Whisper 有更低词错率、更好的语言识别与准确率；[Transcriptions API](https://platform.openai.com/docs/api-reference/audio/createTranscription)支持指定语言、prompt 和流式返回。模型页采用音频/文本 token 计费，实际每分钟成本需用代表性中文样本测算。它适合“录完再转写”的低集成成本路线，但音频会离开设备。
4. **可选云候选：Azure Speech 实时/快速转写（`zh-CN`）。** [语言支持表](https://learn.microsoft.com/en-au/azure/ai-services/speech-service/language-support)明确支持普通话简体中文、短音频快速转写、短语列表；[隐私文档](https://learn.microsoft.com/en-us/azure/foundry/responsible-ai/speech-service/speech-to-text/data-privacy-security)说明实时请求只在 Azure 服务器内存处理且不落盘。Azure 公共零售价格 API 在 East Asia 的 S1 标准 Speech to Text 当前为 **US$1/音频小时**，但区域、合同与中国区价格可能不同，应在发布地区重新核价。

短名单不包含 `whisper.cpp tiny` 作为默认质量候选，也不把任何本地模型塞入主安装包。最终决定应由同一批中文短录音、同一台低配 Windows 机器上的准确率、端到端延迟和峰值内存数据作出。

## 范围与场景假设

- Windows 11 x64，个人使用；优先普通话简体中文，兼顾常见中英混说。
- 用户点击开始、再次点击结束；结果一次性插入当前光标。MVP 不需要边说边显示最终文本。
- 典型单段 3–30 秒，单说话人、近讲麦克风；需要覆盖静音、背景噪声、专有名词和断网。
- 默认不保存原始音频。录制期间允许使用进程内缓冲或仅本次操作可用的临时文件，成功、取消或放弃重试后立即删除。
- “应用本体”只包含 UI、录音、调度和稳定的转写接口；模型、推理运行时以及系统共享运行时分别计量。

## 候选对比

| 候选 | 下载/安装体积（不计应用本体） | CPU / 内存依据 | 中文与延迟依据 | 离线、隐私与成本 | 主要风险 |
| --- | --- | --- | --- | --- | --- |
| SenseVoice INT8 + sherpa-onnx | 官方 INT8 发布资产 `163,002,883` 字节（约 155 MiB）；解压后的 `model.int8.onnx` 官方文档列为 228 MB；当前 Windows x64 MinSizeRel、无 TTS 的共享库发布包为约 5.6 MB 压缩包。模型、运行时均按需下载 | 支持 Windows x64 和 CPU provider。官方没有 Windows x64 RSS/提交内存数据；不得把模型文件大小当成内存数字 | 支持普通话、粤语、英语、日语、韩语；官方模型卡称中文/粤语基准优于 Whisper。sherpa-onnx 在 RK3588 Cortex-A76 INT8 上报告 1 线程 RTF 0.099、2 线程 0.065，仅能证明具备实时余量，不能外推 Windows P95 | 完全离线，音频不离开设备；每次使用无服务费 | 本地占盘最大；官方模型仓库元数据仅标作 `model-license`，正式分发前必须确认具体权重的商业使用与再分发条款；Windows 内存和低端 CPU 延迟未知 |
| whisper.cpp `base-q5_1` | 多语言 `base-q5_1` 为 57 MiB；可选 `tiny-q5_1` 为 31 MiB，但本报告不把它列为质量短名单 | 官方原始 `base` 参考：磁盘 142 MiB、内存约 388 MB；量化版官方只确认减少磁盘/内存，未给精确峰值。支持 Windows、CPU-only、AVX、C API，且高层实现无外部依赖 | Whisper 是多语言模型；官方论文包含普通话语音识别评估，但没有针对 QuickNote 短句/中英混说的可直接套用结果。`whisper.cpp` 提供 benchmark 工具，应在目标机实测 | 完全离线、无调用费；whisper.cpp 与 OpenAI Whisper 代码均为 MIT | 57 MiB 体积有吸引力，但中文质量可能弱于 SenseVoice；量化后的内存和准确率无官方可比数据 |
| OpenAI Transcription API | 本地无需模型；只增加 HTTP/录音编码适配。SDK 不是必需依赖 | 推理在云端；客户端仅承担录音、上传和响应缓冲。服务端没有面向本场景的公开 P95 | `gpt-4o-mini-transcribe` 官方称 WER、语言识别与准确率优于原始 Whisper；API 可指定 ISO-639-1 语言以改善准确率与延迟，并支持 prompt、置信度 logprobs 和流式输出 | 需要联网且音频离开设备。API 数据默认不用于训练；默认滥用监控日志最多可保留 30 天，但 `/v1/audio/transcriptions` 支持零数据保留资格。模型按 token 计费，必须用样本测出“每分钟成本” | 用户要自备/配置凭据或产品承担账单；网络、限流、服务可用性、数据跨境和费用不可忽略 |
| Azure Speech `zh-CN` | 本地无需模型；可直接走 REST，或引入 Speech SDK（体积需随最终技术栈另测） | 云端推理；官方没有适用于 QuickNote 的客户端内存/P95 数字 | 支持 `zh-CN` 实时、快速转写和短语列表。快速转写定位为快于实时、同步且延迟可预期，但没有公开具体 P95 | 需要联网且音频离开设备。官方称实时与快速转写不保留客户数据；East Asia S1 标准价查询值为 US$1/音频小时（约 US$0.0167/分钟），上线区重新核价 | Azure 资源与区域配置增加上手成本；中国区可用性、端点、币种和条款需单独确认 |

### 关于体积口径

- SenseVoice 压缩包字节数来自 sherpa-onnx 官方 GitHub Release API；模型解压大小来自官方预训练模型文档。两者不是同一口径。
- sherpa-onnx 运行时发布包大小来自当前官方 Release 的 Windows x64 `MinSizeRel-no-tts-lib` 资产；实际应用只需哪些 DLL、解压后多大，应由集成原型记录。
- whisper.cpp 的 57/31 MiB 来自维护者发布的官方模型仓库；原始 `base` 的约 388 MB 是运行内存参考，不能当成量化版的峰值保证。
- 云端方案没有本地模型体积，但仍有录音编码、TLS/HTTP 或 SDK 的代码体积；若用 REST，可以避免把大型云 SDK 装入主包。

## 本地候选详析

### SenseVoice INT8 + sherpa-onnx

适配本产品的理由：

- SenseVoice-Small 是非自回归模型，天然适合“录完一小段后快速出整段结果”；官方模型卡的中文基准方向和 QuickNote 的目标语言一致。
- sherpa-onnx 提供稳定 C API/C++ wrapper、Windows x64 支持、SenseVoice 麦克风与 VAD 示例，不要求 Python 运行时。
- 运行时支持指定 `language="zh"` 与逆文本规范化（ITN）。对以中文为主的短句，应固定 `zh`，而不是让极短语音先做自动语言识别；这是一项需用中英混说样本验证的实现假设。
- 官方在 ARM CPU 上的 RTF 已低于 0.1（Cortex-A76 单线程），说明模型架构具备较大实时余量；但桌面端完整耗时还包含录音收尾、音频预处理、模型首次加载和文本后处理。

必须验证的风险：

- **内存**：官方没有 Windows x64 的峰值工作集；模型 228 MB 不等于工作集。若模型常驻，显然会破坏后台空闲内存预算。因此推理必须放在独立按需进程中，完成后退出并释放全部内存。
- **首次使用**：约 155 MiB 压缩下载不适合静默进行。设置页应显示大小、下载进度、校验与删除按钮，并允许只配置云转写。
- **准确率**：官方基准不是 QuickNote 的真实语料。专有名词、口语省略、短句和中英混说都必须在自有样本上测 CER/可接受率。
- **许可**：[sherpa-onnx 源码](https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE)为 Apache-2.0，[SenseVoice 代码仓库](https://github.com/FunAudioLLM/SenseVoice/blob/main/LICENSE)为 MIT，但 Hugging Face 的权重页只显示 `model-license`，未在模型卡正文中给出可核对的完整条款。正式再分发前必须确认具体模型权重的商业使用、修改和再分发授权；不能以推理运行时或代码仓库的许可证推定权重许可证。

### whisper.cpp `base-q5_1`

适配本产品的理由：

- 57 MiB 模型显著小于 SenseVoice INT8，且模型参数、mel filters、词表与权重可装在单一 GGML 文件中，下载与校验简单。
- C/C++ 实现、C API、Windows MSVC/MinGW 与 CPU-only 均为官方支持；可以构建只包含所需功能的原生 sidecar。
- 代码与权重来源的许可较清晰：[whisper.cpp](https://github.com/ggml-org/whisper.cpp/blob/master/LICENSE)与[ OpenAI Whisper](https://github.com/openai/whisper/blob/main/LICENSE)均使用 MIT。发布时仍需携带许可证文本。

必须验证的风险：

- 官方只给原始 `base` 约 388 MB 内存，未给 `base-q5_1` 峰值；也没有能直接代表普通话短句的官方 Windows x64 延迟数据。
- 量化会影响质量，且 SenseVoice 官方模型卡在中文/粤语基准上报告优势。故 `base-q5_1` 是必要的体积基线和备选，不是无需对比即可选定的默认。
- `tiny-q5_1` 仅 31 MiB，可作为极限体积实验，但没有证据证明其中文质量足以进入 MVP，暂不列入最终短名单。

## 云端候选详析

### OpenAI Transcription API

- 对 MVP 的“点击结束后提交整段”流程，使用 `/v1/audio/transcriptions` 即可；无需维持实时 WebSocket。若要边上传边返回，`gpt-4o` 转写模型支持流式响应，而 `whisper-1` 不支持。
- 请求应明确传 `language="zh"`，并可用 prompt 提供产品名或便签上下文中的专有名词；官方 API 说明明确称指定语言可改善准确率与延迟。
- `gpt-4o-mini-transcribe` 是成本/质量短名单；`gpt-4o-transcribe` 可在同一语料上作为质量上界。两者官方定价以音频输入 token 与文本输出 token 计费，因此不能在无样本的情况下承诺固定每分钟价格；原型必须记录每段返回的 usage 并换算成本。
- 隐私界面必须在第一次启用前明确说明音频会发送给 OpenAI。[官方数据控制文档](https://developers.openai.com/api/docs/guides/your-data#default-usage-policies-by-endpoint)说明 API 数据默认不用于训练；但普通账户默认可能生成最多保留 30 天的滥用监控日志。只有具备资格且获批的客户才能启用 Zero Data Retention。

### Azure Speech

- 对短片段可选“快速转写”同步 API；若产品希望结束后立即逐步显示，也可用实时 Speech-to-Text。`zh-CN` 支持快速转写、定制文本/短语列表等能力。
- [实时和快速转写的官方隐私说明](https://learn.microsoft.com/en-us/azure/foundry/responsible-ai/speech-service/speech-to-text/data-privacy-security)优于泛化表述：Microsoft 不保留或存储客户提供的数据；实时音频只在服务器内存处理，传输中加密。仍需在 UI 中明确音频离开设备、处理区域及供应商。
- 可用 REST 避免 SDK 体积；若最终桌面技术栈发现 Speech SDK 对音频设备、重试或代理支持更可靠，再单独测 SDK 对安装包与内存的增量。
- 2026-08-12 通过 [Azure 官方 Retail Prices API](https://prices.azure.com/api/retail/prices) 查询 East Asia 的 S1 标准 Speech to Text 为 US$1/小时。此数字只用于量级比较；实际账单受区域、币种、合同、免费额度和中国区产品差异影响。

## 失败与降级设计

统一转写接口应区分 `local` 与 `cloud` provider，但保持同一操作状态机：

1. 录音结束后冻结光标锚点，并保留一份**仅本次操作可用**的音频缓冲；正文仍可继续编辑。
2. 首选 provider 转写成功后，把文本插入冻结锚点（若编辑器已变化则使用稳定 selection/bookmark 规则），随后立即清除音频缓冲。
3. 麦克风无权限、无有效语音或解码失败：不调用云端，不插入空文本；给出“重录”和打开系统麦克风设置。
4. 本地模型未下载/损坏/内存不足：提示下载、修复或删除模型；若用户已显式启用云转写，提供“本次改用云端”，绝不静默上传。
5. 云端断网、超时、401/429/5xx：指数退避只做有限次数；保留本次音频供“重试”或“改用本地”，用户关闭/取消后删除。正文自动保存不依赖转写成功。
6. 转写结果低置信或为空：展示为可编辑预览，不覆盖已有文本；允许重录。不同 provider 的置信度不可直接比较，因此 MVP 只用于提示，不设跨模型统一阈值。
7. 应用崩溃后的临时音频：启动时清理超时临时文件。若实现可完全在内存中完成，则优先不落盘；若编码库需要文件，放在应用私有临时目录并以最短生命周期删除。

## MVP 原型验证门槛

在最终选型前，用同一套至少 100 条、每条 3–30 秒的样本测试全部短名单。样本至少包含：安静普通话、风扇/键盘噪声、中英混说、数字日期、网址、QuickNote 相关专有名词、轻口音、极短句、静音与非语音。

建议记录：

| 指标 | 建议门槛/记录方式 |
| --- | --- |
| 中文质量 | 报字符错误率（CER）及人工“一次可用/需小改/不可用”比例；默认候选“一次可用”建议 ≥ 90% |
| 热转写延迟 | 从停止录音到文本可插入；5 秒与 30 秒语音分别报 P50/P95。建议 5 秒片段 P95 ≤ 800 ms，30 秒片段 P95 ≤ 2 s |
| 冷转写延迟 | 包含 sidecar 启动与模型加载，单独报 P50/P95；不要混进应用冷启动指标 |
| 本地峰值内存 | 记录推理 sidecar 的 Peak Working Set / Commit，及退出后主进程回落值。sidecar 不得在后台常驻 |
| 体积 | 分列主安装包、转写运行时压缩/解压、模型压缩/解压、共享系统运行时 |
| CPU | 低配四核机器记录峰值、平均值和 UI 是否掉帧；限制推理线程，确保录入窗口仍可编辑 |
| 云成本与可靠性 | 记录每段 usage/账单、上传字节、端到端 P95、超时/429/5xx 比例；分别测直连、代理与断网 |
| 隐私行为 | 验证未启用云端时零网络请求；成功、失败、取消和崩溃恢复后均无遗留音频 |

淘汰条件：本地候选若无法在目标低配机稳定避免 UI 卡顿、峰值内存不可接受或中文质量明显不足，则不作为默认；云候选若在目标发布地区不可用、隐私条款无法接受或 P95 不稳定，则只保留用户自带配置或移出 MVP。

## 建议的决策顺序

1. 先做统一录音与 provider 接口，并用固定 WAV 样本跑无 UI 的基准程序。
2. 对比 SenseVoice INT8 与 whisper.cpp `base-q5_1`；只有在质量足够的前提下比较体积与内存，避免“更小但不可用”。
3. 用同一语料测 `gpt-4o-mini-transcribe`、`gpt-4o-transcribe` 和 Azure `zh-CN`；记录真实成本与本地网络 P95。
4. 最后选择一个默认本地候选和至多一个可选云 provider。若本地候选达不到门槛，MVP 可以先提供“用户主动配置的云转写”，同时保证纯文本能力完全离线；反之亦然。

## 一手来源

- [SenseVoice-Small 官方模型卡：中文基准与计算效率](https://huggingface.co/FunAudioLLM/SenseVoiceSmall)
- [sherpa-onnx SenseVoice 官方预训练模型文档：文件大小、调用方式与 RK3588 RTF](https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html)
- [sherpa-onnx 官方 C API 文档](https://k2-fsa.github.io/sherpa/onnx/c-api/html/index.html)
- [sherpa-onnx 官方 Release：Windows x64 运行时资产](https://github.com/k2-fsa/sherpa-onnx/releases/latest)
- [sherpa-onnx 官方 ASR 模型 Release：SenseVoice 压缩资产](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models)
- [sherpa-onnx Apache-2.0 许可证](https://github.com/k2-fsa/sherpa-onnx/blob/master/LICENSE)
- [SenseVoice 官方仓库与 MIT 许可证](https://github.com/FunAudioLLM/SenseVoice)
- [whisper.cpp 官方 README：Windows/CPU 支持、内存与量化](https://github.com/ggml-org/whisper.cpp/blob/master/README.md)
- [whisper.cpp 官方模型清单：量化模型体积](https://huggingface.co/ggerganov/whisper.cpp)
- [whisper.cpp MIT 许可证](https://github.com/ggml-org/whisper.cpp/blob/master/LICENSE)
- [OpenAI Whisper 论文：多语言训练与普通话评估](https://cdn.openai.com/papers/whisper.pdf)
- [OpenAI Whisper MIT 许可证](https://github.com/openai/whisper/blob/main/LICENSE)
- [OpenAI `gpt-4o-mini-transcribe` 模型页](https://developers.openai.com/api/docs/models/gpt-4o-mini-transcribe)
- [OpenAI Audio Transcriptions API：语言、prompt、stream 与 usage](https://platform.openai.com/docs/api-reference/audio/createTranscription)
- [OpenAI API 数据控制与保留](https://developers.openai.com/api/docs/guides/your-data#default-usage-policies-by-endpoint)
- [Azure Speech-to-Text 概览](https://learn.microsoft.com/en-us/azure/ai-services/speech-service/speech-to-text)
- [Azure Speech 语言支持：`zh-CN`](https://learn.microsoft.com/en-au/azure/ai-services/speech-service/language-support)
- [Azure Speech-to-Text 数据、隐私与安全](https://learn.microsoft.com/en-us/azure/foundry/responsible-ai/speech-service/speech-to-text/data-privacy-security)
- [Azure Retail Prices API](https://prices.azure.com/api/retail/prices)
