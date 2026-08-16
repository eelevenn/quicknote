//! 固定本地包清单、完整校验、自检和原子 current 切换。

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

use crate::platform::PlatformServices;

/// 编译进应用的固定包清单。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    /// 清单结构版本。
    pub schema_version: u32,
    /// 模型与运行时组合的稳定身份。
    pub package_id: String,
    /// 用户可见名称。
    pub display_name: String,
    /// 固定 SenseVoice 模型版本。
    pub model_version: String,
    /// 固定 sherpa-onnx 运行时版本。
    pub runtime_version: String,
    /// sidecar JSONL 协议版本。
    pub sidecar_protocol_version: u32,
    /// 包内运行时二进制目录。
    pub runtime_bin: PathBuf,
    /// 由 QuickNote 构建并复制到运行时目录的 sidecar 文件名。
    pub sidecar_file: String,
    /// 包内 SenseVoice ONNX 相对路径。
    pub model_file: PathBuf,
    /// 包内词表相对路径。
    pub tokens_file: PathBuf,
    /// 必须进入发布说明的已知质量事实。
    pub quality_risk: String,
    /// 只允许下载的固定上游资产。
    pub assets: Vec<PackageAsset>,
    /// 解压后再次核验的关键文件。
    pub verified_files: Vec<VerifiedPackageFile>,
}

impl PackageManifest {
    /// 解析随当前应用发布的不可变清单。
    pub fn bundled() -> Result<Self, PackageError> {
        serde_json::from_str(include_str!("../../assets/transcription-package.json"))
            .map_err(|error| PackageError::Manifest(error.to_string()))
    }

    /// 返回用户确认下载前展示的总压缩字节数。
    pub fn compressed_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.compressed_bytes).sum()
    }

    /// 返回不含 QuickNote sidecar 的解压字节数。
    pub fn extracted_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.extracted_bytes).sum()
    }
}

/// 清单中的一个固定上游资产。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageAsset {
    /// 稳定资产名。
    pub name: String,
    /// 固定上游版本。
    pub version: String,
    /// 上游源提交；模型发布包可能没有独立提交值。
    pub source_commit: Option<String>,
    /// 只允许 HTTPS 的固定下载地址。
    pub url: String,
    /// 下载缓存文件名。
    pub file_name: String,
    /// 资产解压到包内的相对目录。
    pub install_directory: PathBuf,
    /// 下载文件的精确字节数。
    pub compressed_bytes: u64,
    /// 解压目录全部普通文件的精确字节数。
    pub extracted_bytes: u64,
    /// 下载文件的固定 SHA-256。
    pub sha256: String,
    /// 上游声明的许可证。
    pub license: String,
    /// 发布前仍需完成的人工复核说明。
    pub license_status: String,
}

/// 解压后必须匹配的关键文件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPackageFile {
    /// 相对于包根目录的安全路径。
    pub path: PathBuf,
    /// 精确字节数。
    pub bytes: u64,
    /// 固定 SHA-256。
    pub sha256: String,
}

/// 下载、校验和切换过程的稳定阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    /// 等待包管理独占锁。
    Preparing,
    /// 正在下载固定资产。
    Downloading,
    /// 正在核验大小和 SHA-256。
    Verifying,
    /// 正在解压到隔离 staging。
    Extracting,
    /// 正在加载模型并执行本地自检。
    SelfTesting,
    /// 正在原子切换 current 指针。
    Activating,
    /// 新包已成为 current。
    Completed,
}

/// 可由 UI 轮询或通过通道接收的安装进度。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallProgress {
    /// 当前阶段。
    pub phase: InstallPhase,
    /// 当前资产名；非资产阶段为 `None`。
    pub asset: Option<String>,
    /// 已完成字节数。
    pub completed_bytes: u64,
    /// 本阶段或整个下载的总字节数。
    pub total_bytes: u64,
    /// 用户可见说明。
    pub message: String,
}

/// 安装与转写操作共用的轻量取消令牌。
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// 请求当前操作尽快终止。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// 返回用户是否已经请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), PackageError> {
        if self.is_cancelled() {
            Err(PackageError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// 平台层提供下载、解压、sidecar 复制和自检，核心保留切换不变量。
pub trait PackageTools: Send + Sync {
    /// 下载固定资产并回报该资产的已写字节；实现必须响应取消并禁止非 HTTPS 重定向。
    fn download(
        &self,
        asset: &PackageAsset,
        destination: &Path,
        cancellation: &Cancellation,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), PackageError>;

    /// 把已验证归档解压到新的隔离目录。
    fn extract(
        &self,
        archive: &Path,
        destination: &Path,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError>;

    /// 把当前 QuickNote 版本构建的 sidecar 复制到包运行时目录。
    fn prepare_sidecar(
        &self,
        package_root: &Path,
        manifest: &PackageManifest,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError>;

    /// 完整加载模型并执行一次隔离本地推理。
    fn self_test(
        &self,
        package_root: &Path,
        manifest: &PackageManifest,
        cancellation: &Cancellation,
    ) -> Result<(), PackageError>;
}

/// 本地包操作的稳定错误。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PackageError {
    /// 编译内或落盘清单不可解析。
    #[error("本地转写包清单无效：{0}")]
    Manifest(String),
    /// 用户取消下载、修复或自检。
    #[error("本地转写包操作已取消")]
    Cancelled,
    /// 固定资产下载失败。
    #[error("本地转写包下载失败：{0}")]
    Download(String),
    /// 大小、哈希、目录或 inventory 校验失败。
    #[error("本地转写包校验失败：{0}")]
    Verification(String),
    /// 解压失败。
    #[error("本地转写包解压失败：{0}")]
    Extraction(String),
    /// 模型或 sidecar 自检失败。
    #[error("本地转写包自检失败：{0}")]
    SelfTest(String),
    /// 本地文件系统操作失败。
    #[error("本地转写包文件操作失败：{0}")]
    Io(String),
    /// current 指针未能原子更新。
    #[error("本地转写包切换失败：{0}")]
    Activation(String),
}

/// 当前包在设置 UI 中的可观察状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageStatus {
    /// 用户尚未下载任何可用包。
    Missing,
    /// current 指针或 inventory 不再可信。
    Corrupt {
        /// inventory、指针或关键文件的明确错误。
        reason: String,
    },
    /// current 包经过完整 inventory 校验。
    Ready {
        /// 固定模型与运行时组合身份。
        package_id: String,
        /// 当前 generation 的实际磁盘字节数。
        installed_bytes: u64,
    },
}

/// 启动 sidecar 所需的已验证绝对路径。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePaths {
    /// 当前 generation 根目录。
    pub package_root: PathBuf,
    /// 包内运行时 bin 目录。
    pub runtime_bin: PathBuf,
    /// QuickNote sidecar。
    pub sidecar: PathBuf,
    /// SenseVoice INT8 ONNX。
    pub model: PathBuf,
    /// SenseVoice 词表。
    pub tokens: PathBuf,
}

/// 记录模型下载流量和音频零外发声明；真实零网络由发布脚本另行观测。
#[derive(Clone)]
pub struct NetworkAudit {
    path: PathBuf,
}

impl NetworkAudit {
    /// 使用应用私有目录内的 JSONL 审计文件。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 记录一个已验证模型或运行时下载。
    pub fn record_model_download(&self, asset: &PackageAsset) -> Result<(), PackageError> {
        self.append(&NetworkAuditEvent {
            recorded_at_ms: now_ms(),
            category: "model_download",
            operation_id: asset.name.clone(),
            bytes: asset.compressed_bytes,
            source: Some(asset.url.clone()),
        })
    }

    /// 每次转写结束都记录音频外发字节为零，供观测结果交叉核对。
    pub fn record_zero_audio_egress(&self, operation_id: &str) -> Result<(), PackageError> {
        self.append(&NetworkAuditEvent {
            recorded_at_ms: now_ms(),
            category: "audio_egress",
            operation_id: operation_id.to_owned(),
            bytes: 0,
            source: None,
        })
    }

    fn append(&self, event: &NetworkAuditEvent) -> Result<(), PackageError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| PackageError::Io("网络审计文件缺少父目录".to_owned()))?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io_error)?;
        serde_json::to_writer(&mut file, event)
            .map_err(|error| PackageError::Io(error.to_string()))?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_data())
            .map_err(io_error)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkAuditEvent<'a> {
    recorded_at_ms: i64,
    category: &'a str,
    operation_id: String,
    bytes: u64,
    source: Option<String>,
}

/// 保证 staging 完整验证后才切换 current 的包管理器。
pub struct PackageInstaller {
    root: PathBuf,
    manifest: PackageManifest,
    audit: NetworkAudit,
}

impl PackageInstaller {
    /// 在应用数据目录下创建固定包管理器。
    pub fn new(root: impl Into<PathBuf>, manifest: PackageManifest) -> Self {
        let root = root.into();
        let audit_root = root.parent().unwrap_or(&root);
        Self {
            // 包下载与纯转写写入同一分类日志，便于和外部网络观测对齐。
            audit: NetworkAudit::new(audit_root.join("network-audit.jsonl")),
            root,
            manifest,
        }
    }

    /// 返回固定清单。
    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }

    /// 返回当前包状态；损坏状态不会被伪装成未安装。
    pub fn status(&self) -> PackageStatus {
        match self.current_paths_and_record() {
            Ok(Some((paths, record))) => {
                match verify_inventory(&paths.package_root, &record.inventory, None) {
                    Ok(bytes) => PackageStatus::Ready {
                        package_id: record.package_id,
                        installed_bytes: bytes,
                    },
                    Err(error) => PackageStatus::Corrupt {
                        reason: error.to_string(),
                    },
                }
            }
            Ok(None) => PackageStatus::Missing,
            Err(error) => PackageStatus::Corrupt {
                reason: error.to_string(),
            },
        }
    }

    /// 返回完整校验后的当前 sidecar、模型与词表路径。
    pub fn current_paths(&self) -> Result<PackagePaths, PackageError> {
        let (paths, record) = self
            .current_paths_and_record()?
            .ok_or_else(|| PackageError::Verification("尚未安装本地转写包".to_owned()))?;
        verify_inventory(&paths.package_root, &record.inventory, None)?;
        Ok(paths)
    }

    /// 未安装时下载，已安装且健康时直接返回。
    pub fn install(
        &self,
        platform: &dyn PlatformServices,
        tools: &dyn PackageTools,
        cancellation: &Cancellation,
        progress: impl FnMut(InstallProgress),
    ) -> Result<PackagePaths, PackageError> {
        if matches!(self.status(), PackageStatus::Ready { .. }) {
            return self.current_paths();
        }
        self.install_generation(platform, tools, cancellation, progress)
    }

    /// 无论当前是否健康，都建立新 generation 并在自检后切换。
    pub fn repair(
        &self,
        platform: &dyn PlatformServices,
        tools: &dyn PackageTools,
        cancellation: &Cancellation,
        progress: impl FnMut(InstallProgress),
    ) -> Result<PackagePaths, PackageError> {
        self.install_generation(platform, tools, cancellation, progress)
    }

    /// 显式删除 current 包；不会触碰非 current 的旧版本目录。
    pub fn remove_current(&self) -> Result<(), PackageError> {
        let _lock = self.acquire_lock()?;
        let pointer_path = self.current_pointer();
        if !pointer_path.exists() {
            return Ok(());
        }
        let pointer: CurrentPackageRecord = match read_json(&pointer_path) {
            Ok(pointer) => pointer,
            Err(_) => {
                // 损坏指针无法安全定位 generation，只删除指针而不猜测目录目标。
                return fs::remove_file(pointer_path).map_err(io_error);
            }
        };
        if verify_safe_relative_path(&pointer.relative_path).is_err()
            || pointer.relative_path.parent() != Some(Path::new("installed"))
        {
            return fs::remove_file(pointer_path).map_err(io_error);
        }
        let package_root = self.root.join(&pointer.relative_path);
        if !package_root.exists() {
            return fs::remove_file(pointer_path).map_err(io_error);
        }
        let tombstone = self.root.join("removed").join(format!(
            "{}-{}",
            self.manifest.package_id,
            Uuid::now_v7()
        ));
        fs::create_dir_all(tombstone.parent().expect("固定路径有父目录")).map_err(io_error)?;
        fs::rename(&package_root, &tombstone).map_err(io_error)?;
        if let Err(error) = fs::remove_file(pointer_path) {
            let _ = fs::rename(&tombstone, &package_root);
            return Err(io_error(error));
        }
        fs::remove_dir_all(tombstone).map_err(io_error)
    }

    fn install_generation(
        &self,
        platform: &dyn PlatformServices,
        tools: &dyn PackageTools,
        cancellation: &Cancellation,
        mut progress: impl FnMut(InstallProgress),
    ) -> Result<PackagePaths, PackageError> {
        validate_manifest(&self.manifest)?;
        cancellation.check()?;
        fs::create_dir_all(&self.root).map_err(io_error)?;
        let _lock = self.acquire_lock()?;
        let total_download = self.manifest.compressed_bytes();
        progress(InstallProgress {
            phase: InstallPhase::Preparing,
            asset: None,
            completed_bytes: 0,
            total_bytes: total_download,
            message: "正在准备隔离 staging".to_owned(),
        });

        let stage = ScopedDirectory::new(
            self.root
                .join("staging")
                .join(format!("install-{}", Uuid::now_v7())),
        )?;
        let downloads = stage.path().join("downloads");
        let package = stage.path().join("package");
        fs::create_dir_all(&downloads).map_err(io_error)?;
        fs::create_dir_all(&package).map_err(io_error)?;

        let mut completed_download = 0;
        for asset in &self.manifest.assets {
            cancellation.check()?;
            progress(InstallProgress {
                phase: InstallPhase::Downloading,
                asset: Some(asset.name.clone()),
                completed_bytes: completed_download,
                total_bytes: total_download,
                message: format!("正在下载 {}", asset.name),
            });
            let archive = downloads.join(&asset.file_name);
            let mut report_asset_progress = |asset_bytes: u64| {
                progress(InstallProgress {
                    phase: InstallPhase::Downloading,
                    asset: Some(asset.name.clone()),
                    completed_bytes: completed_download + asset_bytes.min(asset.compressed_bytes),
                    total_bytes: total_download,
                    message: format!("正在下载 {}", asset.name),
                });
            };
            tools.download(asset, &archive, cancellation, &mut report_asset_progress)?;
            progress(InstallProgress {
                phase: InstallPhase::Verifying,
                asset: Some(asset.name.clone()),
                completed_bytes: completed_download,
                total_bytes: total_download,
                message: format!("正在校验 {}", asset.name),
            });
            verify_file(
                &archive,
                asset.compressed_bytes,
                &asset.sha256,
                cancellation,
            )?;
            self.audit.record_model_download(asset)?;
            completed_download += asset.compressed_bytes;

            let destination = package.join(&asset.install_directory);
            fs::create_dir_all(&destination).map_err(io_error)?;
            progress(InstallProgress {
                phase: InstallPhase::Extracting,
                asset: Some(asset.name.clone()),
                completed_bytes: completed_download,
                total_bytes: total_download,
                message: format!("正在解压 {}", asset.name),
            });
            tools.extract(&archive, &destination, cancellation)?;
            validate_tree_safety(&destination)?;
            let extracted = directory_bytes(&destination, cancellation)?;
            if extracted != asset.extracted_bytes {
                return Err(PackageError::Verification(format!(
                    "{} 解压体积应为 {} 字节，实际为 {} 字节",
                    asset.name, asset.extracted_bytes, extracted
                )));
            }
        }

        for expected in &self.manifest.verified_files {
            verify_safe_relative_path(&expected.path)?;
            verify_file(
                &package.join(&expected.path),
                expected.bytes,
                &expected.sha256,
                cancellation,
            )?;
        }
        tools.prepare_sidecar(&package, &self.manifest, cancellation)?;
        validate_tree_safety(&package)?;
        progress(InstallProgress {
            phase: InstallPhase::SelfTesting,
            asset: None,
            completed_bytes: total_download,
            total_bytes: total_download,
            message: "正在加载模型并执行本地自检".to_owned(),
        });
        tools.self_test(&package, &self.manifest, cancellation)?;
        cancellation.check()?;

        let inventory = create_inventory(&package, cancellation)?;
        let record = InstalledPackageRecord {
            schema_version: 1,
            package_id: self.manifest.package_id.clone(),
            model_version: self.manifest.model_version.clone(),
            runtime_version: self.manifest.runtime_version.clone(),
            installed_at_ms: now_ms(),
            self_test: "passed".to_owned(),
            quality_risk: self.manifest.quality_risk.clone(),
            inventory,
        };
        write_synced_json(&package.join("package.json"), &record)?;

        progress(InstallProgress {
            phase: InstallPhase::Activating,
            asset: None,
            completed_bytes: total_download,
            total_bytes: total_download,
            message: "正在原子切换本地转写包".to_owned(),
        });
        let previous = self.current_paths_and_record().ok().flatten();
        let generation = format!("{}-{}", self.manifest.package_id, Uuid::now_v7());
        let installed_root = self.root.join("installed");
        fs::create_dir_all(&installed_root).map_err(io_error)?;
        let activated_root = installed_root.join(&generation);
        fs::rename(&package, &activated_root).map_err(io_error)?;
        let pointer = CurrentPackageRecord {
            schema_version: 1,
            package_id: self.manifest.package_id.clone(),
            relative_path: PathBuf::from("installed").join(&generation),
            switched_at_ms: now_ms(),
            self_test: "passed".to_owned(),
        };
        let pointer_bytes = serde_json::to_vec_pretty(&pointer)
            .map_err(|error| PackageError::Activation(error.to_string()))?;
        if let Err(error) = platform.write_file_atomically(&self.current_pointer(), &pointer_bytes)
        {
            let _ = fs::remove_dir_all(&activated_root);
            return Err(PackageError::Activation(error.to_string()));
        }

        if let Some((old_paths, old_record)) = previous
            && old_record.package_id == self.manifest.package_id
            && old_paths.package_root != activated_root
        {
            let _ = fs::remove_dir_all(old_paths.package_root);
        }
        let paths = package_paths(&activated_root, &self.manifest);
        progress(InstallProgress {
            phase: InstallPhase::Completed,
            asset: None,
            completed_bytes: total_download,
            total_bytes: total_download,
            message: "本地转写包已验证并启用".to_owned(),
        });
        Ok(paths)
    }

    fn acquire_lock(&self) -> Result<PackageLock, PackageError> {
        fs::create_dir_all(&self.root).map_err(io_error)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("package.lock"))
            .map_err(io_error)?;
        file.lock_exclusive().map_err(io_error)?;
        Ok(PackageLock(file))
    }

    fn current_pointer(&self) -> PathBuf {
        self.root.join("current.json")
    }

    fn current_paths_and_record(
        &self,
    ) -> Result<Option<(PackagePaths, InstalledPackageRecord)>, PackageError> {
        let pointer_path = self.current_pointer();
        if !pointer_path.exists() {
            return Ok(None);
        }
        let pointer: CurrentPackageRecord = read_json(&pointer_path)?;
        if pointer.schema_version != 1 || pointer.self_test != "passed" {
            return Err(PackageError::Verification(
                "current 指针版本或自检状态无效".to_owned(),
            ));
        }
        verify_safe_relative_path(&pointer.relative_path)?;
        let package_root = self.root.join(&pointer.relative_path);
        let record: InstalledPackageRecord = read_json(&package_root.join("package.json"))?;
        if record.schema_version != 1
            || record.package_id != pointer.package_id
            || record.model_version != self.manifest.model_version
            || record.runtime_version != self.manifest.runtime_version
            || record.self_test != "passed"
        {
            return Err(PackageError::Verification(
                "current 与安装记录不一致".to_owned(),
            ));
        }
        Ok(Some((package_paths(&package_root, &self.manifest), record)))
    }
}

struct PackageLock(File);

impl Drop for PackageLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

struct ScopedDirectory(PathBuf);

impl ScopedDirectory {
    fn new(path: PathBuf) -> Result<Self, PackageError> {
        fs::create_dir_all(&path).map_err(io_error)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScopedDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentPackageRecord {
    schema_version: u32,
    package_id: String,
    relative_path: PathBuf,
    switched_at_ms: i64,
    self_test: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledPackageRecord {
    schema_version: u32,
    package_id: String,
    model_version: String,
    runtime_version: String,
    installed_at_ms: i64,
    self_test: String,
    quality_risk: String,
    inventory: Vec<InventoryEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InventoryEntry {
    path: PathBuf,
    bytes: u64,
    sha256: String,
}

fn package_paths(root: &Path, manifest: &PackageManifest) -> PackagePaths {
    let runtime_bin = root.join(&manifest.runtime_bin);
    PackagePaths {
        package_root: root.to_owned(),
        sidecar: runtime_bin.join(&manifest.sidecar_file),
        runtime_bin,
        model: root.join(&manifest.model_file),
        tokens: root.join(&manifest.tokens_file),
    }
}

fn validate_manifest(manifest: &PackageManifest) -> Result<(), PackageError> {
    if manifest.schema_version != 1 || manifest.assets.len() != 2 {
        return Err(PackageError::Manifest(
            "只支持包含运行时和模型的 v1 清单".to_owned(),
        ));
    }
    if manifest.model_version != "2024-07-17" || manifest.runtime_version != "v1.13.5" {
        return Err(PackageError::Manifest(
            "模型或运行时版本偏离 Issue #21 固定边界".to_owned(),
        ));
    }
    for path in [
        &manifest.runtime_bin,
        &manifest.model_file,
        &manifest.tokens_file,
    ] {
        verify_safe_relative_path(path)?;
    }
    for asset in &manifest.assets {
        if !asset.url.starts_with("https://") {
            return Err(PackageError::Manifest(format!(
                "{} 下载地址不是 HTTPS",
                asset.name
            )));
        }
        verify_safe_relative_path(&asset.install_directory)?;
        let archive_path = Path::new(&asset.file_name);
        verify_safe_relative_path(archive_path)?;
        if archive_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            return Err(PackageError::Manifest(format!(
                "{} 下载文件名不能包含目录",
                asset.name
            )));
        }
        if asset.sha256.len() != 64 || asset.compressed_bytes == 0 || asset.extracted_bytes == 0 {
            return Err(PackageError::Manifest(format!(
                "{} 缺少固定大小或 SHA-256",
                asset.name
            )));
        }
    }
    Ok(())
}

fn verify_safe_relative_path(path: &Path) -> Result<(), PackageError> {
    let safe = !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if safe {
        Ok(())
    } else {
        Err(PackageError::Verification(format!(
            "包路径越界：{}",
            path.display()
        )))
    }
}

fn validate_tree_safety(root: &Path) -> Result<(), PackageError> {
    let canonical_root = root.canonicalize().map_err(io_error)?;
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
            if metadata.file_type().is_symlink() {
                return Err(PackageError::Verification(format!(
                    "包内不允许符号链接：{}",
                    entry.path().display()
                )));
            }
            let canonical = entry.path().canonicalize().map_err(io_error)?;
            if !canonical.starts_with(&canonical_root) {
                return Err(PackageError::Verification(format!(
                    "包文件逃逸 staging：{}",
                    entry.path().display()
                )));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if !metadata.is_file() {
                return Err(PackageError::Verification(format!(
                    "包内只允许普通文件和目录：{}",
                    entry.path().display()
                )));
            }
        }
    }
    Ok(())
}

fn verify_file(
    path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    cancellation: &Cancellation,
) -> Result<(), PackageError> {
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() || metadata.len() != expected_bytes {
        return Err(PackageError::Verification(format!(
            "{} 应为 {} 字节，实际为 {} 字节",
            path.display(),
            expected_bytes,
            metadata.len()
        )));
    }
    let actual = sha256_file(path, Some(cancellation))?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(PackageError::Verification(format!(
            "{} 的 SHA-256 不匹配",
            path.display()
        )));
    }
    Ok(())
}

fn sha256_file(path: &Path, cancellation: Option<&Cancellation>) -> Result<String, PackageError> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let bytes = file.read(&mut buffer).map_err(io_error)?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn directory_bytes(root: &Path, cancellation: &Cancellation) -> Result<u64, PackageError> {
    let mut total = 0_u64;
    for path in regular_files(root)? {
        cancellation.check()?;
        total = total
            .checked_add(fs::metadata(path).map_err(io_error)?.len())
            .ok_or_else(|| PackageError::Verification("解压体积溢出".to_owned()))?;
    }
    Ok(total)
}

fn create_inventory(
    package_root: &Path,
    cancellation: &Cancellation,
) -> Result<Vec<InventoryEntry>, PackageError> {
    let mut inventory = Vec::new();
    for path in regular_files(package_root)? {
        cancellation.check()?;
        let relative = path
            .strip_prefix(package_root)
            .map_err(|error| PackageError::Verification(error.to_string()))?
            .to_owned();
        inventory.push(InventoryEntry {
            bytes: fs::metadata(&path).map_err(io_error)?.len(),
            sha256: sha256_file(&path, Some(cancellation))?,
            path: relative,
        });
    }
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(inventory)
}

fn verify_inventory(
    package_root: &Path,
    inventory: &[InventoryEntry],
    cancellation: Option<&Cancellation>,
) -> Result<u64, PackageError> {
    if inventory.is_empty() {
        return Err(PackageError::Verification("安装 inventory 为空".to_owned()));
    }
    let mut expected_paths = inventory
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    expected_paths.sort();
    let mut actual_paths = regular_files(package_root)?
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(package_root).ok()?.to_owned();
            (relative != Path::new("package.json")).then_some(relative)
        })
        .collect::<Vec<_>>();
    actual_paths.sort();
    if actual_paths != expected_paths {
        return Err(PackageError::Verification(
            "包文件集合与安装 inventory 不一致".to_owned(),
        ));
    }
    let mut total = 0_u64;
    for entry in inventory {
        verify_safe_relative_path(&entry.path)?;
        let path = package_root.join(&entry.path);
        let metadata = fs::metadata(&path).map_err(io_error)?;
        if metadata.len() != entry.bytes || sha256_file(&path, cancellation)? != entry.sha256 {
            return Err(PackageError::Verification(format!(
                "{} 已损坏",
                entry.path.display()
            )));
        }
        total += entry.bytes;
    }
    Ok(total)
}

fn regular_files(root: &Path) -> Result<Vec<PathBuf>, PackageError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let metadata = entry.metadata().map_err(io_error)?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn write_synced_json<T: Serialize>(path: &Path, value: &T) -> Result<(), PackageError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| PackageError::Io(error.to_string()))?;
    bytes.push(b'\n');
    let mut file = File::create(path).map_err(io_error)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(io_error)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, PackageError> {
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes).map_err(|error| PackageError::Verification(error.to_string()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn io_error(error: std::io::Error) -> PackageError {
    PackageError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::TestPlatformServices;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    struct FakeTools {
        runtime_archive: Vec<u8>,
        model_archive: Vec<u8>,
        runtime_file: Vec<u8>,
        model_file: Vec<u8>,
        tokens_file: Vec<u8>,
        fail_self_test: AtomicBool,
    }

    impl PackageTools for FakeTools {
        fn download(
            &self,
            asset: &PackageAsset,
            destination: &Path,
            cancellation: &Cancellation,
            progress: &mut dyn FnMut(u64),
        ) -> Result<(), PackageError> {
            cancellation.check()?;
            let bytes = if asset.name == "runtime" {
                &self.runtime_archive
            } else {
                &self.model_archive
            };
            fs::write(destination, bytes).map_err(io_error)?;
            progress(bytes.len() as u64);
            Ok(())
        }

        fn extract(
            &self,
            archive: &Path,
            destination: &Path,
            cancellation: &Cancellation,
        ) -> Result<(), PackageError> {
            cancellation.check()?;
            if archive.file_name().and_then(|name| name.to_str()) == Some("runtime.tar") {
                fs::create_dir_all(destination.join("bin")).map_err(io_error)?;
                fs::write(destination.join("bin/runtime.dll"), &self.runtime_file).map_err(io_error)
            } else {
                fs::create_dir_all(destination.join("sensevoice")).map_err(io_error)?;
                fs::write(destination.join("sensevoice/model.onnx"), &self.model_file)
                    .and_then(|_| {
                        fs::write(destination.join("sensevoice/tokens.txt"), &self.tokens_file)
                    })
                    .map_err(io_error)
            }
        }

        fn prepare_sidecar(
            &self,
            package_root: &Path,
            manifest: &PackageManifest,
            _cancellation: &Cancellation,
        ) -> Result<(), PackageError> {
            fs::write(
                package_root
                    .join(&manifest.runtime_bin)
                    .join(&manifest.sidecar_file),
                b"sidecar",
            )
            .map_err(io_error)
        }

        fn self_test(
            &self,
            _package_root: &Path,
            _manifest: &PackageManifest,
            _cancellation: &Cancellation,
        ) -> Result<(), PackageError> {
            if self.fail_self_test.load(Ordering::Acquire) {
                Err(PackageError::SelfTest("injected".to_owned()))
            } else {
                Ok(())
            }
        }
    }

    fn fixture() -> (PackageManifest, FakeTools) {
        let runtime_archive = b"runtime archive".to_vec();
        let model_archive = b"model archive".to_vec();
        let runtime_file = b"runtime".to_vec();
        let model_file = b"model".to_vec();
        let tokens_file = b"tokens".to_vec();
        let manifest = PackageManifest {
            schema_version: 1,
            package_id: "sensevoice-fixture".to_owned(),
            display_name: "fixture".to_owned(),
            model_version: "2024-07-17".to_owned(),
            runtime_version: "v1.13.5".to_owned(),
            sidecar_protocol_version: 1,
            runtime_bin: PathBuf::from("runtime/bin"),
            sidecar_file: "sidecar.exe".to_owned(),
            model_file: PathBuf::from("model/sensevoice/model.onnx"),
            tokens_file: PathBuf::from("model/sensevoice/tokens.txt"),
            quality_risk: "79%".to_owned(),
            assets: vec![
                fixture_asset(
                    "runtime",
                    "runtime.tar",
                    &runtime_archive,
                    runtime_file.len(),
                ),
                fixture_asset(
                    "model",
                    "model.tar",
                    &model_archive,
                    model_file.len() + tokens_file.len(),
                ),
            ],
            verified_files: vec![
                fixture_file("model/sensevoice/model.onnx", &model_file),
                fixture_file("model/sensevoice/tokens.txt", &tokens_file),
            ],
        };
        let tools = FakeTools {
            runtime_archive,
            model_archive,
            runtime_file,
            model_file,
            tokens_file,
            fail_self_test: AtomicBool::new(false),
        };
        (manifest, tools)
    }

    fn fixture_asset(name: &str, file: &str, bytes: &[u8], extracted: usize) -> PackageAsset {
        PackageAsset {
            name: name.to_owned(),
            version: "fixed".to_owned(),
            source_commit: None,
            url: format!("https://example.invalid/{file}"),
            file_name: file.to_owned(),
            install_directory: PathBuf::from(name),
            compressed_bytes: bytes.len() as u64,
            extracted_bytes: extracted as u64,
            sha256: digest(bytes),
            license: "test".to_owned(),
            license_status: "test".to_owned(),
        }
    }

    fn fixture_file(path: &str, bytes: &[u8]) -> VerifiedPackageFile {
        VerifiedPackageFile {
            path: PathBuf::from(path),
            bytes: bytes.len() as u64,
            sha256: digest(bytes),
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn bundled_manifest_keeps_fixed_versions_hashes_and_sizes() {
        let manifest = PackageManifest::bundled().expect("解析固定清单");
        validate_manifest(&manifest).expect("校验固定清单");
        assert_eq!(manifest.model_version, "2024-07-17");
        assert_eq!(manifest.runtime_version, "v1.13.5");
        assert_eq!(manifest.compressed_bytes(), 185_928_513);
        assert_eq!(manifest.extracted_bytes(), 303_151_829);
    }

    #[test]
    fn self_test_failure_keeps_previous_generation_current() {
        let directory = TempDir::new().expect("创建临时目录");
        let platform = TestPlatformServices::new(directory.path());
        let (manifest, tools) = fixture();
        let installer = PackageInstaller::new(directory.path().join("packages"), manifest);
        let first = installer
            .install(&platform, &tools, &Cancellation::default(), |_| {})
            .expect("安装第一代");
        tools.fail_self_test.store(true, Ordering::Release);

        installer
            .repair(&platform, &tools, &Cancellation::default(), |_| {})
            .expect_err("注入自检失败");
        assert_eq!(installer.current_paths().expect("读取旧 generation"), first);
        assert!(first.package_root.exists());
    }

    #[test]
    fn cancellation_never_creates_current_pointer() {
        let directory = TempDir::new().expect("创建临时目录");
        let platform = TestPlatformServices::new(directory.path());
        let (manifest, tools) = fixture();
        let installer = PackageInstaller::new(directory.path().join("packages"), manifest);
        let cancellation = Cancellation::default();
        cancellation.cancel();

        assert_eq!(
            installer.install(&platform, &tools, &cancellation, |_| {}),
            Err(PackageError::Cancelled)
        );
        assert_eq!(installer.status(), PackageStatus::Missing);
    }

    #[test]
    fn post_install_corruption_is_reported_and_never_used() {
        let directory = TempDir::new().expect("创建临时目录");
        let platform = TestPlatformServices::new(directory.path());
        let (manifest, tools) = fixture();
        let installer = PackageInstaller::new(directory.path().join("packages"), manifest);
        let paths = installer
            .install(&platform, &tools, &Cancellation::default(), |_| {})
            .expect("安装 fixture");
        fs::write(&paths.model, b"corrupt").expect("损坏模型");

        assert!(matches!(installer.status(), PackageStatus::Corrupt { .. }));
        assert!(installer.current_paths().is_err());
    }

    #[test]
    fn corrupt_current_package_can_still_be_removed_explicitly() {
        let directory = TempDir::new().expect("创建临时目录");
        let platform = TestPlatformServices::new(directory.path());
        let (manifest, tools) = fixture();
        let installer = PackageInstaller::new(directory.path().join("packages"), manifest);
        let paths = installer
            .install(&platform, &tools, &Cancellation::default(), |_| {})
            .expect("安装 fixture");
        fs::write(&paths.model, b"corrupt").expect("损坏模型");

        installer.remove_current().expect("显式删除损坏包");
        assert_eq!(installer.status(), PackageStatus::Missing);
        assert!(!paths.package_root.exists());
    }
}
