//! Windows 录音、固定资产工具和按需 SenseVoice sidecar 协调器。

use quicknote_app::platform::PlatformServices;
use quicknote_app::transcription::{
    Cancellation, ErrorKind, InstallProgress, NetworkAudit, PackageAsset, PackageError,
    PackageInstaller, PackageManifest, PackagePaths, PackageStatus, PackageTools, RetryPolicy,
    SidecarCommand, SidecarEvent, SpeechGate, TempAudioStore, TranscriptionBackend,
    TranscriptionError, TranscriptionExecutor, TranscriptionPreview, TranscriptionState,
    TranscriptionTarget, normalize_transcript, write_pcm16_wave,
};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;
use windows::Win32::Media::Audio::{
    CALLBACK_NULL, HWAVEIN, WAVE_FORMAT_PCM, WAVE_MAPPER, WAVEFORMATEX, WAVEHDR, WHDR_DONE,
    waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart,
    waveInStop, waveInUnprepareHeader,
};
use windows::core::PSTR;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SAMPLE_RATE_HZ: usize = 16_000;
const MAX_SAMPLES: usize = SAMPLE_RATE_HZ * 60;
const SIDECAR_LOAD_TIMEOUT: Duration = Duration::from_secs(25);
const SIDECAR_INFERENCE_TIMEOUT: Duration = Duration::from_secs(15);

/// 使用 Windows 自带 curl/tar 的轻量包工具，避免把网络栈装入主应用。
pub struct WindowsPackageTools {
    bundled_sidecar: PathBuf,
    asset_cache: Option<PathBuf>,
}

impl WindowsPackageTools {
    /// sidecar 与主程序同目录，由生产构建脚本生成。
    pub fn discover() -> Result<Self, PackageError> {
        let executable = std::env::current_exe().map_err(package_io)?;
        let bundled_sidecar = executable
            .parent()
            .ok_or_else(|| PackageError::Io("主程序路径缺少父目录".to_owned()))?
            .join("quicknote-sensevoice-sidecar.exe");
        // 验收或离线部署可显式提供已下载资产；后续仍执行完整大小、SHA 和自检。
        let asset_cache = std::env::var_os("QUICKNOTE_TRANSCRIPTION_ASSET_CACHE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            bundled_sidecar,
            asset_cache,
        })
    }

    fn copy_cached_asset(
        &self,
        source: &Path,
        destination: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), PackageError> {
        let mut input = File::open(source).map_err(package_io)?;
        let mut output = File::create(destination).map_err(package_io)?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        loop {
            if cancellation.is_cancelled() {
                return Err(PackageError::Cancelled);
            }
            let count = input.read(&mut buffer).map_err(package_io)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(package_io)?;
            copied = copied.saturating_add(count as u64);
            progress(copied);
        }
        output.sync_all().map_err(package_io)
    }

    fn run_tool(
        &self,
        mut command: Command,
        cancellation: &Cancellation,
        error: impl Fn(String) -> PackageError,
        mut tick: impl FnMut(),
    ) -> Result<(), PackageError> {
        command
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|cause| error(cause.to_string()))?;
        loop {
            tick();
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PackageError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    let mut stderr = String::new();
                    if let Some(mut stream) = child.stderr.take() {
                        let _ = stream.read_to_string(&mut stderr);
                    }
                    let detail = if stderr.trim().is_empty() {
                        format!("退出码 {status}")
                    } else {
                        stderr.trim().to_owned()
                    };
                    return Err(error(detail));
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(cause) => return Err(error(cause.to_string())),
            }
        }
    }
}

impl PackageTools for WindowsPackageTools {
    fn download(
        &self,
        asset: &PackageAsset,
        destination: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), PackageError> {
        if let Some(cache) = &self.asset_cache {
            let source = cache.join(&asset.file_name);
            if source.is_file() {
                let result = self.copy_cached_asset(&source, destination, cancellation, progress);
                if result.is_err() {
                    let _ = fs::remove_file(destination);
                }
                return result;
            }
        }
        let mut command = Command::new("curl.exe");
        command.args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--output",
        ]);
        command.arg(destination).arg(&asset.url);
        let result = self.run_tool(command, cancellation, PackageError::Download, || {
            // curl 直接写入 staging，文件长度即可提供真实的可取消下载进度。
            if let Ok(metadata) = fs::metadata(destination) {
                progress(metadata.len());
            }
        });
        if result.is_err() {
            let _ = fs::remove_file(destination);
        }
        result
    }

    fn extract(
        &self,
        archive: &Path,
        destination: &Path,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError> {
        let mut command = Command::new("tar.exe");
        command.arg("-xjf").arg(archive).arg("-C").arg(destination);
        self.run_tool(command, cancellation, PackageError::Extraction, || {})
    }

    fn prepare_sidecar(
        &self,
        package_root: &Path,
        manifest: &PackageManifest,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError> {
        if cancellation.is_cancelled() {
            return Err(PackageError::Cancelled);
        }
        if !self.bundled_sidecar.is_file() {
            return Err(PackageError::SelfTest(format!(
                "缺少生产 sidecar：{}；请使用 scripts/build-windows.ps1 构建",
                self.bundled_sidecar.display()
            )));
        }
        let destination = package_root
            .join(&manifest.runtime_bin)
            .join(&manifest.sidecar_file);
        fs::copy(&self.bundled_sidecar, &destination).map_err(package_io)?;
        // Windows 的 FlushFileBuffers 需要可写句柄；只读 File::open 会返回拒绝访问。
        fs::OpenOptions::new()
            .write(true)
            .open(destination)
            .and_then(|file| file.sync_all())
            .map_err(package_io)
    }

    fn self_test(
        &self,
        package_root: &Path,
        manifest: &PackageManifest,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError> {
        let paths = paths_from_manifest(package_root, manifest);
        let wave = package_root.join(".quicknote-self-test.wav");
        write_pcm16_wave(&wave, &vec![0; SAMPLE_RATE_HZ])
            .map_err(|error| PackageError::SelfTest(error.to_string()))?;
        let result = (|| {
            let mut backend = SenseVoiceBackend::start(paths, cancellation.clone())
                .map_err(|error| PackageError::SelfTest(error.to_string()))?;
            backend
                .transcribe("package-self-test", &wave)
                .map(|_| ())
                .map_err(|error| PackageError::SelfTest(error.to_string()))
        })();
        let _ = fs::remove_file(wave);
        result
    }
}

/// 包管理后台线程发送给设置 UI 的事件。
#[derive(Clone, Debug)]
pub enum PackageEvent {
    /// 下载、校验、自检或切换进度。
    Progress(InstallProgress),
    /// current 包状态已刷新。
    Status(PackageStatus),
    /// 当前后台操作失败。
    Failed(PackageError),
}

/// 在后台完成大文件下载和哈希，避免阻塞 Slint 事件循环。
pub struct PackageController {
    root: PathBuf,
    tools: Arc<WindowsPackageTools>,
    active: Arc<Mutex<Option<Cancellation>>>,
    sender: mpsc::Sender<PackageEvent>,
    receiver: Mutex<mpsc::Receiver<PackageEvent>>,
}

impl PackageController {
    /// 创建与当前应用版本绑定的包控制器。
    pub fn new(data_directory: &Path) -> Result<Self, PackageError> {
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            root: data_directory.join("transcription").join("packages"),
            tools: Arc::new(WindowsPackageTools::discover()?),
            active: Arc::new(Mutex::new(None)),
            sender,
            receiver: Mutex::new(receiver),
        })
    }

    /// 在后台刷新完整 inventory 状态，避免启动时阻塞 UI。
    pub fn refresh_status(&self) -> Result<(), PackageError> {
        let mut active = self
            .active
            .lock()
            .map_err(|error| PackageError::Io(error.to_string()))?;
        if active.is_some() {
            return Err(PackageError::Io("已有本地包操作正在进行".to_owned()));
        }
        *active = Some(Cancellation::default());
        drop(active);

        let root = self.root.clone();
        let sender = self.sender.clone();
        let active = Arc::clone(&self.active);
        let spawn = thread::Builder::new()
            .name("quicknote-transcription-package-status".to_owned())
            .spawn(move || {
                let result = PackageManifest::bundled()
                    .map(|manifest| PackageInstaller::new(root, manifest).status());
                match result {
                    Ok(status) => {
                        let _ = sender.send(PackageEvent::Status(status));
                    }
                    Err(error) => {
                        let _ = sender.send(PackageEvent::Failed(error));
                    }
                }
                if let Ok(mut slot) = active.lock() {
                    *slot = None;
                }
            });
        if let Err(error) = spawn {
            if let Ok(mut slot) = self.active.lock() {
                *slot = None;
            }
            return Err(PackageError::Io(error.to_string()));
        }
        Ok(())
    }

    /// 启动下载或强制修复；同一时刻只允许一个包操作。
    pub fn start<P>(&self, platform: P, repair: bool) -> Result<(), PackageError>
    where
        P: PlatformServices + Clone + Send + Sync + 'static,
    {
        let mut active = self
            .active
            .lock()
            .map_err(|error| PackageError::Io(error.to_string()))?;
        if active.is_some() {
            return Err(PackageError::Io("已有本地包操作正在进行".to_owned()));
        }
        let cancellation = Cancellation::default();
        *active = Some(cancellation.clone());
        drop(active);

        let root = self.root.clone();
        let tools = Arc::clone(&self.tools);
        let sender = self.sender.clone();
        let active = Arc::clone(&self.active);
        let spawn = thread::Builder::new()
            .name("quicknote-transcription-package".to_owned())
            .spawn(move || {
                let result = PackageManifest::bundled().and_then(|manifest| {
                    let installer = PackageInstaller::new(&root, manifest);
                    let send_progress = |progress| {
                        let _ = sender.send(PackageEvent::Progress(progress));
                    };
                    if repair {
                        installer.repair(&platform, tools.as_ref(), &cancellation, send_progress)
                    } else {
                        installer.install(&platform, tools.as_ref(), &cancellation, send_progress)
                    }
                    .map(|_| installer.status())
                });
                match result {
                    Ok(status) => {
                        let _ = sender.send(PackageEvent::Status(status));
                    }
                    Err(error) => {
                        let _ = sender.send(PackageEvent::Failed(error));
                    }
                }
                if let Ok(mut slot) = active.lock() {
                    *slot = None;
                }
            });
        if let Err(error) = spawn {
            if let Ok(mut slot) = self.active.lock() {
                *slot = None;
            }
            return Err(PackageError::Io(error.to_string()));
        }
        Ok(())
    }

    /// 在后台显式删除 current generation。
    pub fn remove(&self) -> Result<(), PackageError> {
        let mut active = self
            .active
            .lock()
            .map_err(|error| PackageError::Io(error.to_string()))?;
        if active.is_some() {
            return Err(PackageError::Io("已有本地包操作正在进行".to_owned()));
        }
        let cancellation = Cancellation::default();
        *active = Some(cancellation);
        drop(active);

        let root = self.root.clone();
        let sender = self.sender.clone();
        let active = Arc::clone(&self.active);
        let spawn = thread::Builder::new()
            .name("quicknote-transcription-package-remove".to_owned())
            .spawn(move || {
                let result = PackageManifest::bundled().and_then(|manifest| {
                    let installer = PackageInstaller::new(root, manifest);
                    installer.remove_current()?;
                    Ok(installer.status())
                });
                match result {
                    Ok(status) => {
                        let _ = sender.send(PackageEvent::Status(status));
                    }
                    Err(error) => {
                        let _ = sender.send(PackageEvent::Failed(error));
                    }
                }
                if let Ok(mut slot) = active.lock() {
                    *slot = None;
                }
            });
        if let Err(error) = spawn {
            if let Ok(mut slot) = self.active.lock() {
                *slot = None;
            }
            return Err(PackageError::Io(error.to_string()));
        }
        Ok(())
    }

    /// 取消当前下载、解压或自检。
    pub fn cancel(&self) {
        if let Ok(active) = self.active.lock()
            && let Some(cancellation) = active.as_ref()
        {
            cancellation.cancel();
        }
    }

    /// 返回当前是否有包后台操作。
    pub fn is_busy(&self) -> bool {
        self.active.lock().is_ok_and(|active| active.is_some())
    }

    /// 非阻塞取出设置 UI 事件。
    pub fn drain_events(&self) -> Vec<PackageEvent> {
        let Ok(receiver) = self.receiver.lock() else {
            return Vec::new();
        };
        receiver.try_iter().collect()
    }
}

fn paths_from_manifest(root: &Path, manifest: &PackageManifest) -> PackagePaths {
    let runtime_bin = root.join(&manifest.runtime_bin);
    PackagePaths {
        package_root: root.to_owned(),
        sidecar: runtime_bin.join(&manifest.sidecar_file),
        runtime_bin,
        model: root.join(&manifest.model_file),
        tokens: root.join(&manifest.tokens_file),
    }
}

fn package_io(error: std::io::Error) -> PackageError {
    PackageError::Io(error.to_string())
}

struct SidecarProcess {
    child: Child,
    input: ChildStdin,
    events: mpsc::Receiver<Result<SidecarEvent, String>>,
    reader: Option<JoinHandle<()>>,
    cancellation: Cancellation,
}

impl SidecarProcess {
    fn start(paths: &PackagePaths, cancellation: Cancellation) -> Result<Self, TranscriptionError> {
        if !paths.sidecar.is_file() || !paths.model.is_file() || !paths.tokens.is_file() {
            return Err(TranscriptionError::new(
                ErrorKind::ModelMissing,
                "本地转写包缺少 sidecar、模型或词表",
            ));
        }
        let mut child = Command::new(&paths.sidecar)
            .arg(format!("--model={}", paths.model.display()))
            .arg(format!("--tokens={}", paths.tokens.display()))
            .arg("--threads=4")
            .current_dir(&paths.runtime_bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| TranscriptionError::new(ErrorKind::ModelMissing, error.to_string()))?;
        let Some(input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TranscriptionError::new(
                ErrorKind::Protocol,
                "sidecar stdin 未建立",
            ));
        };
        let Some(output) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TranscriptionError::new(
                ErrorKind::Protocol,
                "sidecar stdout 未建立",
            ));
        };
        let (sender, events) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("quicknote-sidecar-reader".to_owned())
            .spawn(move || {
                for line in BufReader::new(output).lines() {
                    let event = match line {
                        Ok(line) => serde_json::from_str(&line).map_err(|error| error.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            });
        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TranscriptionError::new(ErrorKind::Io, error.to_string()));
            }
        };
        let mut process = Self {
            child,
            input,
            events,
            reader: Some(reader),
            cancellation,
        };
        match process.receive(SIDECAR_LOAD_TIMEOUT)? {
            SidecarEvent::Ready {
                protocol_version: 1,
                candidate,
                ..
            } if candidate == "sensevoice" => Ok(process),
            SidecarEvent::Failed { error, .. } => Err(error),
            event => Err(TranscriptionError::new(
                ErrorKind::Protocol,
                format!("sidecar 未返回兼容 ready 事件：{event:?}"),
            )),
        }
    }

    fn send(&mut self, command: &SidecarCommand) -> Result<(), TranscriptionError> {
        serde_json::to_writer(&mut self.input, command)
            .map_err(|error| TranscriptionError::new(ErrorKind::Protocol, error.to_string()))?;
        self.input
            .write_all(b"\n")
            .and_then(|_| self.input.flush())
            .map_err(|error| TranscriptionError::new(ErrorKind::SidecarCrashed, error.to_string()))
    }

    fn receive(&mut self, timeout: Duration) -> Result<SidecarEvent, TranscriptionError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.cancellation.is_cancelled() {
                self.kill();
                return Err(TranscriptionError::new(
                    ErrorKind::Cancelled,
                    "用户取消了本地转写",
                ));
            }
            match self.events.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(event)) => return Ok(event),
                Ok(Err(error)) => {
                    return Err(TranscriptionError::new(ErrorKind::Protocol, error));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.crash_error("sidecar 输出流提前关闭"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                return Err(self.crash_error("sidecar 在返回结果前退出"));
            }
            if Instant::now() >= deadline {
                self.kill();
                return Err(TranscriptionError::new(
                    ErrorKind::Timeout,
                    "本地 sidecar 等待超时",
                ));
            }
        }
    }

    fn crash_error(&mut self, fallback: &str) -> TranscriptionError {
        // stdout 断开但进程仍存活时先终止，避免读取 stderr 一直等待 EOF。
        if self.child.try_wait().ok().flatten().is_none() {
            self.kill();
        }
        let mut detail = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut detail);
        }
        TranscriptionError::new(
            ErrorKind::SidecarCrashed,
            if detail.trim().is_empty() {
                fallback.to_owned()
            } else {
                detail.trim().to_owned()
            },
        )
    }

    fn shutdown(&mut self) {
        let _ = self.send(&SidecarCommand::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            self.kill();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SenseVoiceBackend {
    paths: PackagePaths,
    process: SidecarProcess,
    cancellation: Cancellation,
}

impl SenseVoiceBackend {
    fn start(paths: PackagePaths, cancellation: Cancellation) -> Result<Self, TranscriptionError> {
        let process = SidecarProcess::start(&paths, cancellation.clone())?;
        Ok(Self {
            paths,
            process,
            cancellation,
        })
    }

    /// 模型加载阶段也共享整个操作唯一的一次瞬态重试预算。
    fn start_with_retry(
        paths: PackagePaths,
        cancellation: Cancellation,
    ) -> Result<(Self, usize), TranscriptionError> {
        match Self::start(paths.clone(), cancellation.clone()) {
            Ok(backend) => Ok((backend, 0)),
            Err(error) if error.kind.is_retryable() && !cancellation.is_cancelled() => {
                Self::start(paths, cancellation).map(|backend| (backend, 1))
            }
            Err(error) => Err(error),
        }
    }
}

impl TranscriptionBackend for SenseVoiceBackend {
    fn transcribe(
        &mut self,
        request_id: &str,
        wav_path: &Path,
    ) -> Result<String, TranscriptionError> {
        self.process.send(&SidecarCommand::Transcribe {
            request_id: request_id.to_owned(),
            wav_path: wav_path.to_owned(),
        })?;
        match self.process.receive(SIDECAR_INFERENCE_TIMEOUT)? {
            SidecarEvent::Completed {
                protocol_version: 1,
                request_id: returned_id,
                text,
                ..
            } if returned_id == request_id => Ok(text),
            SidecarEvent::Failed {
                protocol_version: 1,
                request_id: returned_id,
                error,
            } if returned_id
                .as_deref()
                .is_none_or(|value| value == request_id) =>
            {
                Err(error)
            }
            event => Err(TranscriptionError::new(
                ErrorKind::Protocol,
                format!("sidecar 返回了不匹配事件：{event:?}"),
            )),
        }
    }

    fn restart(&mut self) -> Result<(), TranscriptionError> {
        self.process.shutdown();
        self.process = SidecarProcess::start(&self.paths, self.cancellation.clone())?;
        Ok(())
    }
}

/// 发送给 Slint UI 的非阻塞语音事件。
#[derive(Clone, Debug)]
pub enum VoiceEvent {
    /// 状态变化，不会改写编辑器正文或选择范围。
    State {
        /// 稳定生命周期。
        state: TranscriptionState,
        /// 用户可见说明。
        message: String,
    },
    /// 成功结果必须先进入可编辑预览。
    Preview(TranscriptionPreview),
    /// 最终错误；当前录音已在发送前删除。
    Failed(TranscriptionError),
}

struct ActiveVoice {
    id: Uuid,
    stop_recording: Arc<AtomicBool>,
    recording_active: Arc<AtomicBool>,
    cancellation: Cancellation,
    target: Arc<Mutex<TranscriptionTarget>>,
}

/// 在后台线程运行录音与推理，Slint 编辑器始终留在 UI 线程。
pub struct VoiceController {
    package_root: PathBuf,
    operations_root: PathBuf,
    audit: NetworkAudit,
    active: Arc<Mutex<Option<ActiveVoice>>>,
    sender: mpsc::Sender<VoiceEvent>,
    receiver: Mutex<mpsc::Receiver<VoiceEvent>>,
}

impl VoiceController {
    /// 使用应用私有转写目录并清理无锁遗留录音。
    pub fn new(data_directory: &Path) -> Result<Self, TranscriptionError> {
        let transcription_root = data_directory.join("transcription");
        let operations_root = transcription_root.join("operations");
        TempAudioStore::new(&operations_root).recover_stale()?;
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            package_root: transcription_root.join("packages"),
            operations_root,
            audit: NetworkAudit::new(transcription_root.join("network-audit.jsonl")),
            active: Arc::new(Mutex::new(None)),
            sender,
            receiver: Mutex::new(receiver),
        })
    }

    /// 第一次调用开始录音；再次调用冻结结束光标并停止录音。
    pub fn toggle(&self, target: TranscriptionTarget) -> Result<(), TranscriptionError> {
        let mut active = self.active.lock().map_err(lock_error)?;
        if let Some(operation) = active.as_mut() {
            if !operation.recording_active.load(Ordering::Acquire) {
                return Err(TranscriptionError::new(
                    ErrorKind::Io,
                    "当前录音已经结束，正在本地转写",
                ));
            }
            *operation.target.lock().map_err(lock_error)? = target;
            operation.stop_recording.store(true, Ordering::Release);
            return Ok(());
        }

        let id = Uuid::now_v7();
        let stop_recording = Arc::new(AtomicBool::new(false));
        let recording_active = Arc::new(AtomicBool::new(true));
        let cancellation = Cancellation::default();
        let frozen_target = Arc::new(Mutex::new(target));
        *active = Some(ActiveVoice {
            id,
            stop_recording: Arc::clone(&stop_recording),
            recording_active: Arc::clone(&recording_active),
            cancellation: cancellation.clone(),
            target: Arc::clone(&frozen_target),
        });
        drop(active);

        let package_root = self.package_root.clone();
        let operations_root = self.operations_root.clone();
        let audit = self.audit.clone();
        let sender = self.sender.clone();
        let active = Arc::clone(&self.active);
        let spawn = thread::Builder::new()
            .name("quicknote-local-transcription".to_owned())
            .spawn(move || {
                let _ = sender.send(VoiceEvent::State {
                    state: TranscriptionState::Preparing,
                    message: "正在校验本地包并加载 SenseVoice".to_owned(),
                });
                let result = run_voice_operation(
                    id,
                    &package_root,
                    &operations_root,
                    &audit,
                    &stop_recording,
                    &recording_active,
                    &cancellation,
                    &frozen_target,
                    &sender,
                );
                if let Err(error) = result {
                    let state = if error.kind == ErrorKind::Cancelled {
                        TranscriptionState::Cancelled
                    } else {
                        TranscriptionState::Failed
                    };
                    let _ = sender.send(VoiceEvent::State {
                        state,
                        message: error.message.clone(),
                    });
                    let _ = sender.send(VoiceEvent::Failed(error));
                }
                if let Ok(mut slot) = active.lock()
                    && slot.as_ref().is_some_and(|operation| operation.id == id)
                {
                    *slot = None;
                }
            });
        if let Err(error) = spawn {
            if let Ok(mut slot) = self.active.lock() {
                *slot = None;
            }
            return Err(TranscriptionError::new(ErrorKind::Io, error.to_string()));
        }
        Ok(())
    }

    /// 取消录音、模型加载或推理；sidecar 后代进程会被终止。
    pub fn cancel(&self) {
        if let Ok(active) = self.active.lock()
            && let Some(operation) = active.as_ref()
        {
            operation.cancellation.cancel();
            operation.stop_recording.store(true, Ordering::Release);
        }
    }

    /// 返回当前是否存在录音或推理操作。
    pub fn is_busy(&self) -> bool {
        self.active.lock().is_ok_and(|active| active.is_some())
    }

    /// 录音仍在进行时刷新最近的编辑光标，供 60 秒自动结束路径冻结。
    pub fn refresh_target(&self, target: TranscriptionTarget) -> Result<(), TranscriptionError> {
        let mut active = self.active.lock().map_err(lock_error)?;
        if let Some(operation) = active.as_mut()
            && operation.recording_active.load(Ordering::Acquire)
        {
            *operation.target.lock().map_err(lock_error)? = target;
        }
        Ok(())
    }

    /// 返回录音是否仍接受光标更新或显式停止。
    pub fn is_recording(&self) -> bool {
        self.active.lock().is_ok_and(|active| {
            active
                .as_ref()
                .is_some_and(|operation| operation.recording_active.load(Ordering::Acquire))
        })
    }

    /// 非阻塞取出 UI 事件。
    pub fn drain_events(&self) -> Vec<VoiceEvent> {
        let Ok(receiver) = self.receiver.lock() else {
            return Vec::new();
        };
        receiver.try_iter().collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_voice_operation(
    id: Uuid,
    package_root: &Path,
    operations_root: &Path,
    audit: &NetworkAudit,
    stop_recording: &AtomicBool,
    recording_active: &AtomicBool,
    cancellation: &Cancellation,
    target: &Mutex<TranscriptionTarget>,
    sender: &mpsc::Sender<VoiceEvent>,
) -> Result<(), TranscriptionError> {
    struct AudioAuditGuard<'a> {
        audit: &'a NetworkAudit,
        operation_id: String,
    }
    impl Drop for AudioAuditGuard<'_> {
        fn drop(&mut self) {
            let _ = self.audit.record_zero_audio_egress(&self.operation_id);
        }
    }
    let request_id = id.to_string();
    let _audit = AudioAuditGuard {
        audit,
        operation_id: request_id.clone(),
    };
    let manifest = PackageManifest::bundled()
        .map_err(|error| TranscriptionError::new(ErrorKind::ModelCorrupt, error.to_string()))?;
    let installer = PackageInstaller::new(package_root, manifest);
    let paths = installer.current_paths().map_err(|error| {
        let kind = if matches!(
            installer.status(),
            quicknote_app::transcription::PackageStatus::Missing
        ) {
            ErrorKind::ModelMissing
        } else {
            ErrorKind::ModelCorrupt
        };
        TranscriptionError::new(kind, error.to_string())
    })?;
    let model_cancellation = cancellation.clone();
    let model_thread = thread::Builder::new()
        .name("quicknote-sensevoice-load".to_owned())
        .spawn(move || SenseVoiceBackend::start_with_retry(paths, model_cancellation))
        .map_err(|error| TranscriptionError::new(ErrorKind::Io, error.to_string()))?;
    // 任一录音或门禁失败路径都会取消并回收尚在加载的 sidecar。
    let mut model_load = ModelLoad::new(model_thread, cancellation.clone());

    let operation = TempAudioStore::new(operations_root).begin(&request_id)?;
    let _ = sender.send(VoiceEvent::State {
        state: TranscriptionState::Recording,
        message: "正在录音；可继续编辑，单次最长 60 秒".to_owned(),
    });
    let samples = record_default_microphone(stop_recording, cancellation);
    // 此原子位关闭后，UI 不得再改变冻结目标。
    recording_active.store(false, Ordering::Release);
    let samples = samples?;
    operation.write_samples(&samples)?;
    let mut gate = SpeechGate::default();
    gate.require_speech(&samples)?;
    let _ = sender.send(VoiceEvent::State {
        state: TranscriptionState::Transcribing,
        message: "正在本地转写；音频不会发送到网络".to_owned(),
    });

    let (backend, startup_retries) = model_load.finish()?;
    let mut executor = TranscriptionExecutor::new(
        backend,
        RetryPolicy {
            max_retries: 1_usize.saturating_sub(startup_retries),
        },
    );
    let text = normalize_transcript(&executor.execute(&request_id, operation.path())?);
    if text.is_empty() {
        return Err(TranscriptionError::new(
            ErrorKind::NoSpeech,
            "SenseVoice 没有返回可预览文字",
        ));
    }
    let frozen = target.lock().map_err(lock_error)?.clone();
    let _ = sender.send(VoiceEvent::Preview(TranscriptionPreview::new(text, frozen)));
    let _ = sender.send(VoiceEvent::State {
        state: TranscriptionState::Preview,
        message: "转写完成；请检查预览后再插入".to_owned(),
    });
    Ok(())
}

/// 确保提前返回时不会留下正在加载模型的后台 sidecar。
struct ModelLoad {
    handle: Option<JoinHandle<Result<(SenseVoiceBackend, usize), TranscriptionError>>>,
    cancellation: Cancellation,
}

impl ModelLoad {
    fn new(
        handle: JoinHandle<Result<(SenseVoiceBackend, usize), TranscriptionError>>,
        cancellation: Cancellation,
    ) -> Self {
        Self {
            handle: Some(handle),
            cancellation,
        }
    }

    fn finish(&mut self) -> Result<(SenseVoiceBackend, usize), TranscriptionError> {
        self.handle
            .take()
            .expect("模型加载线程只回收一次")
            .join()
            .map_err(|_| {
                TranscriptionError::new(ErrorKind::SidecarCrashed, "SenseVoice 加载线程异常退出")
            })?
    }
}

impl Drop for ModelLoad {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cancellation.cancel();
            let _ = handle.join();
        }
    }
}

fn record_default_microphone(
    stop_recording: &AtomicBool,
    cancellation: &Cancellation,
) -> Result<Vec<i16>, TranscriptionError> {
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: SAMPLE_RATE_HZ as u32,
        nAvgBytesPerSec: (SAMPLE_RATE_HZ * 2) as u32,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let mut handle = HWAVEIN::default();
    // SAFETY: format 在 waveInOpen 同步调用期间有效，回调模式为 NULL。
    mm_result(
        unsafe {
            waveInOpen(
                Some(&mut handle),
                WAVE_MAPPER,
                &format,
                None,
                None,
                CALLBACK_NULL,
            )
        },
        "打开默认麦克风",
    )?;
    let mut capture = WaveInCapture::new(handle)?;
    // SAFETY: capture 的缓冲区和装箱 WAVEHDR 在整个录音生命周期保持稳定。
    mm_result(
        unsafe {
            waveInPrepareHeader(
                capture.handle,
                capture.header.as_mut(),
                std::mem::size_of::<WAVEHDR>() as u32,
            )
        },
        "准备录音缓冲区",
    )?;
    capture.prepared = true;
    mm_result(
        unsafe {
            waveInAddBuffer(
                capture.handle,
                capture.header.as_mut(),
                std::mem::size_of::<WAVEHDR>() as u32,
            )
        },
        "提交录音缓冲区",
    )?;
    mm_result(unsafe { waveInStart(capture.handle) }, "开始录音")?;
    capture.started = true;

    loop {
        if cancellation.is_cancelled() {
            return Err(TranscriptionError::new(
                ErrorKind::Cancelled,
                "用户取消了录音",
            ));
        }
        if stop_recording.load(Ordering::Acquire) || capture.header.dwFlags & WHDR_DONE != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    capture.stop();
    let bytes = capture.header.dwBytesRecorded as usize;
    let sample_count = (bytes / 2).min(capture.samples.len());
    capture.samples.truncate(sample_count);
    Ok(std::mem::take(&mut capture.samples))
}

struct WaveInCapture {
    handle: HWAVEIN,
    samples: Vec<i16>,
    header: Box<WAVEHDR>,
    prepared: bool,
    started: bool,
}

impl WaveInCapture {
    fn new(handle: HWAVEIN) -> Result<Self, TranscriptionError> {
        let mut samples = vec![0_i16; MAX_SAMPLES];
        let header = Box::new(WAVEHDR {
            lpData: PSTR(samples.as_mut_ptr().cast()),
            dwBufferLength: (samples.len() * 2) as u32,
            ..Default::default()
        });
        Ok(Self {
            handle,
            samples,
            header,
            prepared: false,
            started: false,
        })
    }

    fn stop(&mut self) {
        if self.started {
            // SAFETY: handle 仍打开；停止与 reset 会把缓冲区归还给调用方。
            let _ = unsafe { waveInStop(self.handle) };
            let _ = unsafe { waveInReset(self.handle) };
            self.started = false;
        }
    }
}

impl Drop for WaveInCapture {
    fn drop(&mut self) {
        self.stop();
        if self.prepared {
            // SAFETY: WAVEHDR 仍装箱且不再处于驱动队列中。
            let _ = unsafe {
                waveInUnprepareHeader(
                    self.handle,
                    self.header.as_mut(),
                    std::mem::size_of::<WAVEHDR>() as u32,
                )
            };
        }
        // SAFETY: 当前对象唯一拥有 waveIn 句柄。
        let _ = unsafe { waveInClose(self.handle) };
    }
}

fn mm_result(code: u32, operation: &'static str) -> Result<(), TranscriptionError> {
    if code == 0 {
        Ok(())
    } else {
        Err(TranscriptionError::new(
            ErrorKind::Io,
            format!("{operation}失败，WinMM 错误码 {code}"),
        ))
    }
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> TranscriptionError {
    TranscriptionError::new(ErrorKind::Io, format!("转写状态锁不可用：{error}"))
}
