//! 录音格式、无语音判定和瞬态音频清理。

use earshot::Detector;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{ErrorKind, TranscriptionError};

const SAMPLE_RATE_HZ: u32 = 16_000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const VAD_FRAME_SAMPLES: usize = 256;
const MIN_SPEECH_FRAMES: usize = 19;
const MIN_SPEECH_RUN: usize = 6;
const MIN_RMS: f32 = 120.0;
const SPEECH_SCORE: f32 = 0.5;

/// 单次语音输入的绝对上限。
pub const MAX_RECORDING_SECONDS: u32 = 60;

/// 已验证的 16 kHz 单声道 PCM16 波形。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pcm16Wave {
    /// 线性 PCM 样本。
    pub samples: Vec<i16>,
}

impl Pcm16Wave {
    /// 返回录音时长的向上取整毫秒数。
    pub fn duration_ms(&self) -> u64 {
        (self.samples.len() as u64 * 1_000).div_ceil(u64::from(SAMPLE_RATE_HZ))
    }
}

/// 无语音判定的可观察证据。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeechAssessment {
    /// 参与判定的完整 16 ms 帧数。
    pub total_frames: usize,
    /// 同时通过能量和神经 VAD 阈值的帧数。
    pub speech_frames: usize,
    /// 连续语音帧的最长长度。
    pub longest_run: usize,
    /// 本次录音的最高 VAD 分数。
    pub peak_score: f32,
    /// 是否允许进入 SenseVoice 推理。
    pub has_speech: bool,
}

/// 使用 Earshot 神经 VAD 和持续时间滞回，拒绝静音与孤立键盘瞬态。
pub struct SpeechGate {
    detector: Box<Detector>,
}

impl Default for SpeechGate {
    fn default() -> Self {
        Self {
            detector: Detector::default_boxed(),
        }
    }
}

impl SpeechGate {
    /// 分析一个新的录音；每次调用都会重置流式模型状态。
    pub fn assess(&mut self, samples: &[i16]) -> SpeechAssessment {
        self.detector.reset();
        let mut scores = Vec::with_capacity(samples.len() / VAD_FRAME_SAMPLES);
        let mut energized = Vec::with_capacity(scores.capacity());
        for frame in samples.as_chunks::<VAD_FRAME_SAMPLES>().0 {
            let rms = root_mean_square(frame);
            let score = self.detector.predict_i16(frame).clamp(0.0, 1.0);
            scores.push(score);
            energized.push(rms >= MIN_RMS);
        }
        classify_frames(&scores, &energized)
    }

    /// 确保输入包含持续语音，否则返回不得自动重试的确定性错误。
    pub fn require_speech(
        &mut self,
        samples: &[i16],
    ) -> Result<SpeechAssessment, TranscriptionError> {
        let assessment = self.assess(samples);
        if assessment.has_speech {
            Ok(assessment)
        } else {
            Err(TranscriptionError::new(
                ErrorKind::NoSpeech,
                "没有检测到持续人声；静音、键盘声和孤立瞬态不会送入模型",
            ))
        }
    }
}

fn classify_frames(scores: &[f32], energized: &[bool]) -> SpeechAssessment {
    let total_frames = scores.len().min(energized.len());
    let mut speech_frames = 0;
    let mut current_run = 0;
    let mut longest_run = 0;
    let mut peak_score = 0.0_f32;
    for (&score, &has_energy) in scores.iter().zip(energized) {
        peak_score = peak_score.max(score);
        if has_energy && score >= SPEECH_SCORE {
            speech_frames += 1;
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    SpeechAssessment {
        total_frames,
        speech_frames,
        longest_run,
        peak_score,
        has_speech: speech_frames >= MIN_SPEECH_FRAMES && longest_run >= MIN_SPEECH_RUN,
    }
}

fn root_mean_square(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let square_sum = samples.iter().fold(0.0_f64, |sum, sample| {
        let value = f64::from(*sample);
        sum + value * value
    });
    (square_sum / samples.len() as f64).sqrt() as f32
}

/// 把 PCM16 样本写为固定格式 WAV；不会接受超过 60 秒的输入。
pub fn write_pcm16_wave(path: &Path, samples: &[i16]) -> Result<(), TranscriptionError> {
    let maximum_samples = SAMPLE_RATE_HZ as usize * MAX_RECORDING_SECONDS as usize;
    if samples.len() > maximum_samples {
        return Err(TranscriptionError::new(
            ErrorKind::UnsupportedAudio,
            "单次录音不得超过 60 秒",
        ));
    }
    let data_bytes = samples
        .len()
        .checked_mul(2)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| {
            TranscriptionError::new(ErrorKind::UnsupportedAudio, "PCM 数据长度超出 WAV 范围")
        })?;
    let riff_bytes = 36_u32
        .checked_add(data_bytes)
        .ok_or_else(|| TranscriptionError::new(ErrorKind::UnsupportedAudio, "WAV 文件长度溢出"))?;
    let mut file = File::create(path)
        .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
    file.write_all(b"RIFF")
        .and_then(|_| file.write_all(&riff_bytes.to_le_bytes()))
        .and_then(|_| file.write_all(b"WAVEfmt "))
        .and_then(|_| file.write_all(&16_u32.to_le_bytes()))
        .and_then(|_| file.write_all(&1_u16.to_le_bytes()))
        .and_then(|_| file.write_all(&CHANNELS.to_le_bytes()))
        .and_then(|_| file.write_all(&SAMPLE_RATE_HZ.to_le_bytes()))
        .and_then(|_| file.write_all(&(SAMPLE_RATE_HZ * 2).to_le_bytes()))
        .and_then(|_| file.write_all(&2_u16.to_le_bytes()))
        .and_then(|_| file.write_all(&BITS_PER_SAMPLE.to_le_bytes()))
        .and_then(|_| file.write_all(b"data"))
        .and_then(|_| file.write_all(&data_bytes.to_le_bytes()))
        .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())
            .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
    }
    file.sync_all()
        .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))
}

/// 读取并严格验证 16 kHz 单声道 PCM16 WAV。
pub fn read_pcm16_wave(path: &Path) -> Result<Pcm16Wave, TranscriptionError> {
    let mut file = File::open(path)
        .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
    let mut header = [0_u8; 12];
    file.read_exact(&mut header)
        .map_err(|error| TranscriptionError::new(ErrorKind::UnsupportedAudio, error.to_string()))?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err(TranscriptionError::new(
            ErrorKind::UnsupportedAudio,
            "录音不是 RIFF/WAVE 文件",
        ));
    }

    let mut format = None;
    let mut data = None;
    loop {
        let mut chunk = [0_u8; 8];
        match file.read_exact(&mut chunk) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                return Err(TranscriptionError::new(
                    ErrorKind::UnsupportedAudio,
                    error.to_string(),
                ));
            }
        }
        let size = u32::from_le_bytes(chunk[4..8].try_into().expect("切片长度固定")) as usize;
        let mut contents = vec![0_u8; size];
        file.read_exact(&mut contents).map_err(|error| {
            TranscriptionError::new(ErrorKind::UnsupportedAudio, error.to_string())
        })?;
        if size % 2 == 1 {
            file.seek(SeekFrom::Current(1)).map_err(|error| {
                TranscriptionError::new(ErrorKind::UnsupportedAudio, error.to_string())
            })?;
        }
        match &chunk[..4] {
            b"fmt " => format = Some(contents),
            b"data" => data = Some(contents),
            _ => {}
        }
    }

    let format = format
        .ok_or_else(|| TranscriptionError::new(ErrorKind::UnsupportedAudio, "WAV 缺少 fmt 块"))?;
    if format.len() < 16
        || u16::from_le_bytes([format[0], format[1]]) != 1
        || u16::from_le_bytes([format[2], format[3]]) != CHANNELS
        || u32::from_le_bytes(format[4..8].try_into().expect("切片长度固定")) != SAMPLE_RATE_HZ
        || u16::from_le_bytes([format[14], format[15]]) != BITS_PER_SAMPLE
    {
        return Err(TranscriptionError::new(
            ErrorKind::UnsupportedAudio,
            "WAV 必须为 16 kHz、单声道、PCM16",
        ));
    }
    let data = data
        .ok_or_else(|| TranscriptionError::new(ErrorKind::UnsupportedAudio, "WAV 缺少 data 块"))?;
    if data.len() % 2 != 0 {
        return Err(TranscriptionError::new(
            ErrorKind::UnsupportedAudio,
            "PCM16 数据长度必须为偶数",
        ));
    }
    let samples = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect::<Vec<_>>();
    if samples.len() > SAMPLE_RATE_HZ as usize * MAX_RECORDING_SECONDS as usize {
        return Err(TranscriptionError::new(
            ErrorKind::UnsupportedAudio,
            "单次录音不得超过 60 秒",
        ));
    }
    Ok(Pcm16Wave { samples })
}

/// 为每次录音建立带跨进程锁的独立瞬态目录。
pub struct TempAudioStore {
    root: PathBuf,
}

impl TempAudioStore {
    /// 使用 `%LOCALAPPDATA%\\QuickNote\\transcription\\operations` 一类私有目录。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 创建当前操作的唯一目录和空 WAV 目标。
    pub fn begin(&self, request_id: &str) -> Result<TempAudio, TranscriptionError> {
        validate_request_id(request_id)?;
        fs::create_dir_all(&self.root)
            .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
        let directory = self.root.join(request_id);
        fs::create_dir(&directory)
            .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
        let lock_path = directory.join("operation.lock");
        let lock = match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(lock_path)
        {
            Ok(lock) => lock,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(TranscriptionError::new(ErrorKind::Io, error.to_string()));
            }
        };
        if let Err(error) = lock.lock_exclusive() {
            drop(lock);
            let _ = fs::remove_dir_all(&directory);
            return Err(TranscriptionError::new(ErrorKind::Io, error.to_string()));
        }
        Ok(TempAudio {
            audio_path: directory.join("audio.wav"),
            directory,
            lock,
        })
    }

    /// 启动时只删除能够取得独占锁的遗留操作目录。
    pub fn recover_stale(&self) -> Result<Vec<PathBuf>, TranscriptionError> {
        let mut removed = Vec::new();
        if !self.root.exists() {
            return Ok(removed);
        }
        for entry in fs::read_dir(&self.root)
            .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?
        {
            let directory = entry
                .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?
                .path();
            if !directory.is_dir() {
                continue;
            }
            let Ok(lock) = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(directory.join("operation.lock"))
            else {
                continue;
            };
            if lock.try_lock_exclusive().is_ok() {
                FileExt::unlock(&lock)
                    .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
                fs::remove_dir_all(&directory)
                    .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
                removed.push(directory);
            }
        }
        Ok(removed)
    }
}

/// 只在当前操作和最多一次瞬态重试期间存在的 WAV。
pub struct TempAudio {
    directory: PathBuf,
    audio_path: PathBuf,
    lock: File,
}

impl TempAudio {
    /// 返回 recorder 与 sidecar 共用的私有 WAV 路径。
    pub fn path(&self) -> &Path {
        &self.audio_path
    }

    /// 把录音样本写入当前操作文件。
    pub fn write_samples(&self, samples: &[i16]) -> Result<(), TranscriptionError> {
        write_pcm16_wave(&self.audio_path, samples)
    }
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn validate_request_id(request_id: &str) -> Result<(), TranscriptionError> {
    let valid = !request_id.is_empty()
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(TranscriptionError::new(
            ErrorKind::Io,
            "request_id 只能包含 ASCII 字母、数字、短横线和下划线",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn wav_round_trip_preserves_pcm_samples() {
        let directory = TempDir::new().expect("创建临时目录");
        let path = directory.path().join("sample.wav");
        let samples = vec![-20_000, -1, 0, 1, 20_000];
        write_pcm16_wave(&path, &samples).expect("写入 WAV");
        assert_eq!(
            read_pcm16_wave(&path).expect("读取 WAV"),
            Pcm16Wave { samples }
        );
    }

    #[test]
    fn sustained_scores_pass_but_keyboard_impulses_do_not() {
        let speech = classify_frames(&[0.8; 30], &[true; 30]);
        assert!(speech.has_speech);

        let mut scores = vec![0.01; 80];
        let mut energized = vec![false; 80];
        for index in [10, 30, 50, 70] {
            scores[index] = 0.99;
            energized[index] = true;
        }
        let keyboard = classify_frames(&scores, &energized);
        assert!(!keyboard.has_speech);
        assert_eq!(keyboard.longest_run, 1);
    }

    #[test]
    fn actual_silence_is_rejected_before_model_inference() {
        let mut gate = SpeechGate::default();
        let error = gate
            .require_speech(&vec![0; SAMPLE_RATE_HZ as usize])
            .expect_err("静音必须被拒绝");
        assert_eq!(error.kind, ErrorKind::NoSpeech);
    }

    #[test]
    fn temporary_audio_is_removed_on_drop() {
        let directory = TempDir::new().expect("创建临时目录");
        let store = TempAudioStore::new(directory.path().join("operations"));
        let operation = store.begin("request-1").expect("创建操作");
        operation.write_samples(&[0; 256]).expect("写入录音");
        let operation_directory = operation.path().parent().expect("操作目录").to_owned();
        drop(operation);
        assert!(!operation_directory.exists());
    }

    #[test]
    fn startup_recovery_removes_only_unlocked_operation() {
        let directory = TempDir::new().expect("创建临时目录");
        let root = directory.path().join("operations");
        fs::create_dir_all(root.join("stale")).expect("创建遗留目录");
        File::create(root.join("stale").join("operation.lock")).expect("创建遗留锁");
        fs::create_dir_all(root.join("active")).expect("创建活动目录");
        let active_lock =
            File::create(root.join("active").join("operation.lock")).expect("创建活动锁");
        active_lock.lock_exclusive().expect("锁定活动目录");
        // 模拟进程在创建目录后、创建锁文件前崩溃。
        fs::create_dir_all(root.join("orphan-without-lock")).expect("创建无锁遗留目录");

        let mut removed = TempAudioStore::new(&root)
            .recover_stale()
            .expect("恢复遗留录音");
        removed.sort();
        assert_eq!(
            removed,
            vec![root.join("orphan-without-lock"), root.join("stale")]
        );
        assert!(root.join("active").exists());
        FileExt::unlock(&active_lock).expect("解锁活动目录");
    }
}
