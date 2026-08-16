//! 本地转写包、临时音频和 sidecar 生命周期的稳定边界。

mod audio;
mod package;
mod protocol;

pub use audio::{
    MAX_RECORDING_SECONDS, Pcm16Wave, SpeechAssessment, SpeechGate, TempAudio, TempAudioStore,
    read_pcm16_wave, write_pcm16_wave,
};
pub use package::{
    Cancellation, InstallPhase, InstallProgress, NetworkAudit, PackageAsset, PackageError,
    PackageInstaller, PackageManifest, PackagePaths, PackageStatus, PackageTools,
};
pub use protocol::{
    ErrorKind, PROTOCOL_VERSION, RetryPolicy, SidecarCommand, SidecarEvent, TranscriptionBackend,
    TranscriptionError, TranscriptionExecutor, TranscriptionPreview, TranscriptionState,
    TranscriptionTarget, insert_at_frozen_offset, normalize_transcript,
};
