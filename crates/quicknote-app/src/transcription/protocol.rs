//! 平台中立的 sidecar 协议、错误分类和一次重试规则。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

/// QuickNote 与本地 sidecar 共同支持的 JSONL 协议版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// 一次语音输入可观察的生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionState {
    /// 没有进行中的语音输入。
    Idle,
    /// 正在检查本地包并启动 sidecar。
    Preparing,
    /// sidecar 已加载模型，录音仍可继续。
    Ready,
    /// 正在录音。
    Recording,
    /// 正在执行本地推理。
    Transcribing,
    /// 结果等待用户编辑或插入。
    Preview,
    /// 用户取消了当前操作。
    Cancelled,
    /// 当前操作失败。
    Failed,
}

/// 产品层可稳定处理的转写错误类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 尚未安装本地转写包。
    ModelMissing,
    /// 包、模型或运行时校验失败。
    ModelCorrupt,
    /// 录音格式不符合 16 kHz 单声道 PCM16 合同。
    UnsupportedAudio,
    /// 可靠无语音判定拒绝了输入。
    NoSpeech,
    /// sidecar 在返回结果前退出。
    SidecarCrashed,
    /// sidecar 超过当前输入的有限等待时间。
    Timeout,
    /// 用户取消当前操作。
    Cancelled,
    /// 本地文件操作失败。
    Io,
    /// sidecar 返回了不兼容或损坏的协议消息。
    Protocol,
}

impl ErrorKind {
    /// 只有进程崩溃和超时属于允许自动重试一次的瞬态错误。
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::SidecarCrashed | Self::Timeout)
    }
}

/// 不向 UI 泄漏平台错误类型的稳定转写错误。
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct TranscriptionError {
    /// 供重试和展示策略判断的稳定类别。
    pub kind: ErrorKind,
    /// 可记录且可展示的中文说明。
    pub message: String,
}

impl TranscriptionError {
    /// 创建稳定转写错误。
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// 发给按需 sidecar 的一行 JSON 命令。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SidecarCommand {
    /// 对临时 WAV 执行一次本地推理。
    Transcribe {
        /// 当前操作的稳定身份。
        #[serde(rename = "requestId")]
        request_id: String,
        /// 仅位于当前临时操作目录内的 WAV 路径。
        #[serde(rename = "wavPath")]
        wav_path: PathBuf,
    },
    /// 请求 sidecar 正常退出。
    Shutdown,
}

/// sidecar 写回的一行 JSON 事件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SidecarEvent {
    /// 模型加载完成，可以接受转写命令。
    Ready {
        /// 必须与 [`PROTOCOL_VERSION`] 完全一致。
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// 固定为 `sensevoice`。
        candidate: String,
        /// 本次模型加载耗时。
        #[serde(rename = "loadMs")]
        load_ms: f64,
    },
    /// 本地推理成功。
    Completed {
        /// 必须与 [`PROTOCOL_VERSION`] 完全一致。
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// 必须与请求身份一致。
        #[serde(rename = "requestId")]
        request_id: String,
        /// SenseVoice 返回的原始文字。
        text: String,
        /// 不含录音和模型加载的推理耗时。
        #[serde(rename = "inferenceMs")]
        inference_ms: f64,
    },
    /// sidecar 可分类地拒绝请求。
    Failed {
        /// 必须与 [`PROTOCOL_VERSION`] 完全一致。
        #[serde(rename = "protocolVersion")]
        protocol_version: u32,
        /// 启动失败时可能尚无请求身份。
        #[serde(rename = "requestId")]
        request_id: Option<String>,
        /// 稳定错误。
        error: TranscriptionError,
    },
}

/// 语音输入结束时确定的冻结插入目标，避免结果跟随当前焦点漂移。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionTarget {
    /// `None` 表示仍无持久身份的空白草稿。
    pub note_id: Option<Uuid>,
    /// 当前正文中的 UTF-8 字节偏移；插入时会收敛到字符边界。
    pub byte_offset: usize,
}

/// 由于已知人工质量风险，结果先进入可编辑预览。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionPreview {
    /// 用户可在插入前修订的文字。
    pub text: String,
    /// 语音输入结束时确定的冻结插入目标。
    pub target: TranscriptionTarget,
    /// 发布说明必须保留的已知质量事实。
    pub risk_notice: String,
}

impl TranscriptionPreview {
    /// 为每个成功结果建立显式质量提示。
    pub fn new(text: impl Into<String>, target: TranscriptionTarget) -> Self {
        Self {
            text: text.into(),
            target,
            risk_notice: "SenseVoice 人工一次可用率理论上限为 79%，请在插入前检查文字。".to_owned(),
        }
    }
}

/// 抽象 sidecar，使一次重试规则可独立于 Win32 进程实现验证。
pub trait TranscriptionBackend {
    /// 对同一临时 WAV 执行一次推理。
    fn transcribe(
        &mut self,
        request_id: &str,
        wav_path: &Path,
    ) -> Result<String, TranscriptionError>;

    /// 丢弃旧进程并从已验证包重新启动。
    fn restart(&mut self) -> Result<(), TranscriptionError>;
}

/// 瞬态错误的有限重试配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// 额外尝试次数；产品值固定为一次。
    pub max_retries: usize,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 1 }
    }
}

/// 在一个临时音频生命周期内执行确定性的一次重试。
pub struct TranscriptionExecutor<B> {
    backend: B,
    policy: RetryPolicy,
}

impl<B: TranscriptionBackend> TranscriptionExecutor<B> {
    /// 包装一个已经准备好的本地 sidecar 后端。
    pub fn new(backend: B, policy: RetryPolicy) -> Self {
        Self { backend, policy }
    }

    /// 崩溃或超时最多重试一次；确定性错误直接返回。
    pub fn execute(
        &mut self,
        request_id: &str,
        wav_path: &Path,
    ) -> Result<String, TranscriptionError> {
        let mut retries = 0;
        loop {
            match self.backend.transcribe(request_id, wav_path) {
                Ok(text) => return Ok(text),
                Err(error) if error.kind.is_retryable() && retries < self.policy.max_retries => {
                    retries += 1;
                    self.backend.restart()?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// 归还后端，便于测试核对调用次数。
    pub fn into_backend(self) -> B {
        self.backend
    }
}

/// 去除 SenseVoice 控制标签并收敛多余空白，不擅自改写正文内容。
pub fn normalize_transcript(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '<' && characters.peek() == Some(&'|') {
            characters.next();
            let mut previous = '\0';
            for current in characters.by_ref() {
                if previous == '|' && current == '>' {
                    break;
                }
                previous = current;
            }
            continue;
        }
        output.push(character);
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 把用户确认的预览插入冻结偏移，并避免切开 UTF-8 字符。
pub fn insert_at_frozen_offset(body: &str, byte_offset: usize, transcript: &str) -> String {
    let mut offset = byte_offset.min(body.len());
    while offset > 0 && !body.is_char_boundary(offset) {
        offset -= 1;
    }
    if transcript.trim().is_empty() {
        return body.to_owned();
    }

    let before = &body[..offset];
    let after = &body[offset..];
    // 预览是用户最终确认的文本，不擅自增加或删除中英文空格。
    format!("{before}{transcript}{after}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeBackend {
        outcomes: VecDeque<Result<String, TranscriptionError>>,
        calls: usize,
        restarts: usize,
    }

    impl TranscriptionBackend for FakeBackend {
        fn transcribe(
            &mut self,
            _request_id: &str,
            _wav_path: &Path,
        ) -> Result<String, TranscriptionError> {
            self.calls += 1;
            self.outcomes.pop_front().expect("测试必须提供结果")
        }

        fn restart(&mut self) -> Result<(), TranscriptionError> {
            self.restarts += 1;
            Ok(())
        }
    }

    #[test]
    fn transient_failure_retries_exactly_once() {
        let backend = FakeBackend {
            outcomes: VecDeque::from([
                Err(TranscriptionError::new(ErrorKind::SidecarCrashed, "boom")),
                Ok("完成".to_owned()),
            ]),
            calls: 0,
            restarts: 0,
        };
        let mut executor = TranscriptionExecutor::new(backend, RetryPolicy::default());

        assert_eq!(
            executor.execute("r1", Path::new("audio.wav")).unwrap(),
            "完成"
        );
        let backend = executor.into_backend();
        assert_eq!((backend.calls, backend.restarts), (2, 1));
    }

    #[test]
    fn deterministic_failure_does_not_retry() {
        let backend = FakeBackend {
            outcomes: VecDeque::from([Err(TranscriptionError::new(
                ErrorKind::NoSpeech,
                "没有检测到语音",
            ))]),
            calls: 0,
            restarts: 0,
        };
        let mut executor = TranscriptionExecutor::new(backend, RetryPolicy::default());

        assert_eq!(
            executor.execute("r1", Path::new("audio.wav")),
            Err(TranscriptionError::new(
                ErrorKind::NoSpeech,
                "没有检测到语音"
            ))
        );
        let backend = executor.into_backend();
        assert_eq!((backend.calls, backend.restarts), (1, 0));
    }

    #[test]
    fn normalization_removes_only_control_tags() {
        assert_eq!(
            normalize_transcript("<|zh|><|NEUTRAL|><|Speech|>  今天  开会 "),
            "今天 开会"
        );
    }

    #[test]
    fn frozen_offset_never_splits_a_unicode_character() {
        assert_eq!(insert_at_frozen_offset("你好吗", 2, "真的"), "真的你好吗");
        assert_eq!(insert_at_frozen_offset("前后", 3, "中间"), "前中间后");
    }

    #[test]
    fn frozen_insertion_preserves_user_edited_spacing_exactly() {
        assert_eq!(insert_at_frozen_offset("甲乙", 3, " X "), "甲 X 乙");
    }
}
