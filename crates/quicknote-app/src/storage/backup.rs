//! 一致备份、保留期和显式整库恢复；活动数据库始终由单写者拥有。

use super::migrations::{APPLICATION_ID, SUPPORTED_SCHEMA_VERSION, inspect_error};
use crate::{ApplicationConfig, ApplicationError};
use chrono::{Datelike, Local};
use rusqlite::{Connection, MAIN_DB, OpenFlags};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECOVERY_MARKER: &str = "recovery-required";
const DAY_MS: i64 = 86_400_000;

/// 已验证可恢复的整库备份，不包含活动数据库连接。
#[derive(Clone, Debug)]
pub struct BackupInfo {
    /// 备份文件的完整路径。
    pub path: PathBuf,
    /// 文件最后写入时间，UTC Unix 毫秒。
    pub created_at_ms: i64,
}

/// 备份失败不会改变“已自动保存”的数据库提交承诺。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackupStatus {
    /// 最近成功备份的时间。
    pub last_success_at_ms: Option<i64>,
    /// 最近失败原因；下次成功后清除。
    pub warning: Option<String>,
}

/// 持久恢复门禁保证在恢复界面退出后，下次启动不会偷偷创建空库。
pub struct RecoverySession {
    directory: PathBuf,
    /// 原始故障数据库及 WAL/SHM 的隔离目录，恢复后也保留。
    pub quarantine_directory: PathBuf,
    /// 从新到旧排列且通过完整性、身份与版本检查的候选。
    pub backups: Vec<BackupInfo>,
}

impl RecoverySession {
    /// 只能在 Application::open 失败且没有活动连接时调用。
    pub fn prepare(config: &ApplicationConfig) -> Result<Self, ApplicationError> {
        let directory = config.data_directory();
        let marker = directory.join(RECOVERY_MARKER);
        let quarantine_name = if marker.exists() {
            fs::read_to_string(&marker).map_err(|error| io_error("read_recovery_marker", error))?
        } else {
            let name = format!("fault-{}", uuid::Uuid::now_v7());
            fs::create_dir_all(directory.join("recovery").join(&name))
                .map_err(|error| io_error("create_quarantine", error))?;
            // 先落盘门禁再移动任何数据库文件，崩溃后也不会静默变为空库。
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(|error| io_error("create_recovery_marker", error))?;
            file.write_all(name.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|error| io_error("write_recovery_marker", error))?;
            name
        };
        // 标记只能引用 recovery 下的单层目录，拒绝被修改后的路径穿越。
        if !quarantine_name.starts_with("fault-") || quarantine_name.contains(['/', '\\', ':']) {
            return Err(io_error("read_recovery_marker", "恢复目录标记无效"));
        }
        let quarantine_directory = directory.join("recovery").join(quarantine_name);
        fs::create_dir_all(&quarantine_directory)
            .map_err(|error| io_error("create_quarantine", error))?;
        for name in ["quicknote.db", "quicknote.db-wal", "quicknote.db-shm"] {
            let source = directory.join(name);
            if source.exists() {
                // 未完成恢复再次进入时也保留已有故障副本，绝不覆盖。
                let mut target = quarantine_directory.join(name);
                if target.exists() {
                    target = quarantine_directory.join(format!("{}-{name}", uuid::Uuid::now_v7()));
                }
                fs::rename(&source, &target)
                    .map_err(|error| io_error("quarantine_database", error))?;
            }
        }
        let backups = verified_backups(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            quarantine_directory,
            backups,
        })
    }

    /// 从展示过且仍有效的备份恢复；不接受任意路径或损坏文件。
    pub fn restore(&self, backup_path: &Path) -> Result<(), ApplicationError> {
        if !self.backups.iter().any(|backup| backup.path == backup_path) {
            return Err(io_error("restore_backup", "请选择已验证的备份"));
        }
        validate_backup(backup_path)?;
        self.finish(Some(backup_path))
    }

    /// 用户明确确认后新建空库，原始文件仍留在隔离目录。
    pub fn create_empty(&self) -> Result<(), ApplicationError> {
        self.finish(None)
    }

    fn finish(&self, source: Option<&Path>) -> Result<(), ApplicationError> {
        let destination = self.directory.join("quicknote.db");
        if destination.exists() {
            return Err(io_error(
                "restore_backup",
                "目标数据库已存在，请重新打开恢复界面",
            ));
        }
        if let Some(source) = source {
            let temporary = self
                .directory
                .join(format!(".restore-{}.tmp", uuid::Uuid::now_v7()));
            let result = (|| {
                let connection = open_readonly(source)?;
                connection
                    .backup(MAIN_DB, &temporary, None)
                    .map_err(|error| io_error("restore_snapshot", error))?;
                validate_backup(&temporary)?;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&temporary)
                    .and_then(|file| file.sync_all())
                    .map_err(|error| io_error("sync_restore", error))?;
                fs::rename(&temporary, &destination)
                    .map_err(|error| io_error("publish_restore", error))
            })();
            if result.is_err() {
                let _ = fs::remove_file(temporary);
            }
            result?;
        }
        // 只有完整发布成功（或显式选择空库）才允许下一次正常打开。
        fs::remove_file(self.directory.join(RECOVERY_MARKER))
            .map_err(|error| io_error("finish_recovery", error))
    }
}

/// 在正常打开数据库前检查未完成恢复，防止隔离后出现隐式空库。
pub(super) fn require_recovery_if_pending(directory: &Path) -> Result<(), ApplicationError> {
    if directory.join(RECOVERY_MARKER).exists() {
        return Err(ApplicationError::RecoveryRequired);
    }
    Ok(())
}

/// 对备份使用只读连接，验证失败不会改变候选字节。
fn open_readonly(path: &Path) -> Result<Connection, ApplicationError> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| io_error("open_backup", error))
}

/// 健康检查覆盖页完整性及外键关系；原始诊断信息保留给恢复界面。
pub(super) fn check_health(connection: &Connection) -> Result<(), ApplicationError> {
    let check: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        // 暂时锁定和 I/O 失败不可误报损坏，否则会向用户提供不必要的整库恢复。
        .map_err(|error| inspect_error("check_database_pages", error))?;
    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| inspect_error("prepare_foreign_key_check", error))?;
    let has_bad_reference = foreign_keys
        .exists([])
        .map_err(|error| inspect_error("check_foreign_keys", error))?;
    if check != "ok" || has_bad_reference {
        return Err(ApplicationError::DatabaseCorrupt {
            message: format!("{check}; 外键异常：{has_bad_reference}"),
        });
    }
    Ok(())
}

fn validate_backup(path: &Path) -> Result<(), ApplicationError> {
    let connection = open_readonly(path)?;
    let identity: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| io_error("verify_backup_identity", error))?;
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| io_error("verify_backup_version", error))?;
    if identity != APPLICATION_ID || !(1..=SUPPORTED_SCHEMA_VERSION).contains(&version) {
        return Err(io_error("verify_backup", "备份身份不匹配或版本不兼容"));
    }
    check_health(&connection)
}

/// 返回管理目录内的普通 db 文件；不跟随符号链接，也不递归其他目录。
fn backup_files(directory: &Path) -> Result<Vec<BackupInfo>, ApplicationError> {
    let entries = match fs::read_dir(directory.join("backups")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("list_backups", error)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_error("read_backup_entry", error))?;
        if entry
            .file_type()
            .map_err(|error| io_error("read_backup_type", error))?
            .is_file()
            && entry.path().extension().is_some_and(|ext| ext == "db")
        {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(|error| io_error("read_backup_date", error))?;
            files.push(BackupInfo {
                path: entry.path(),
                created_at_ms: timestamp_ms(modified),
            });
        }
    }
    files.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then(b.path.cmp(&a.path))
    });
    Ok(files)
}

/// 列出最近有效候选；坏备份留在磁盘用于诊断，但不会推荐给用户。
pub(crate) fn verified_backups(directory: &Path) -> Result<Vec<BackupInfo>, ApplicationError> {
    Ok(backup_files(directory)?
        .into_iter()
        .filter(|file| validate_backup(&file.path).is_ok())
        .collect())
}

/// 永久清除前先移除所有可能保留正文的自动/迁移备份，失败则阻止清除事务提交。
pub(super) fn clear_history(connection: &Connection) -> Result<(), ApplicationError> {
    let directory = data_directory(connection)?.join("backups");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("clear_backup_history", error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error("read_backup_entry", error))?;
        // 中断的临时快照同样可能包含正文；这里只删除管理目录中的普通文件。
        if entry
            .file_type()
            .map_err(|error| io_error("read_backup_type", error))?
            .is_file()
        {
            fs::remove_file(entry.path())
                .map_err(|error| io_error("clear_backup_history", error))?;
        }
    }
    Ok(())
}

/// 管理线程缓存当日状态，避免每次输入都扫描磁盘。
pub(super) struct BackupMaintenance {
    pub status: BackupStatus,
    day: String,
    retry_after_ms: i64,
    checkpoint_at: std::time::Instant,
}

impl BackupMaintenance {
    pub fn new() -> Self {
        Self {
            status: BackupStatus::default(),
            day: String::new(),
            retry_after_ms: 0,
            checkpoint_at: std::time::Instant::now(),
        }
    }

    /// 普通备份失败只记录警告；显式重试或永久清除后可以绕过一小时退避。
    pub fn after_write(&mut self, connection: &Connection, force: bool) {
        let now = timestamp_ms(SystemTime::now());
        let day = Local::now().format("%Y-%m-%d").to_string();
        if !force && (self.day == day || now < self.retry_after_ms) {
            return;
        }
        match create_daily_backup(connection, &day, force) {
            Ok(timestamp) => {
                self.day = day;
                self.status = BackupStatus {
                    last_success_at_ms: Some(timestamp),
                    warning: None,
                };
                self.retry_after_ms = 0;
            }
            Err(error) => {
                self.day.clear();
                self.status.warning =
                    Some(format!("自动备份失败，正文已保存；一小时后重试：{error}"));
                self.retry_after_ms = now + 3_600_000;
            }
        }
    }

    /// 每 30 秒收敛 WAL；大于 16 MiB 时主动截断。
    pub fn tick(&mut self, connection: &Connection) {
        if self.status.warning.is_some() {
            self.after_write(connection, false);
        }
        if self.checkpoint_at.elapsed() >= Duration::from_secs(30) {
            let large = data_directory(connection)
                .ok()
                .and_then(|dir| fs::metadata(dir.join("quicknote.db-wal")).ok())
                .is_some_and(|metadata| metadata.len() > 16 * 1024 * 1024);
            let _ = connection.execute_batch(if large {
                "PRAGMA wal_checkpoint(TRUNCATE);"
            } else {
                "PRAGMA wal_checkpoint(PASSIVE);"
            });
            self.checkpoint_at = std::time::Instant::now();
        }
    }
}

fn create_daily_backup(
    connection: &Connection,
    day: &str,
    force: bool,
) -> Result<i64, ApplicationError> {
    let directory = data_directory(connection)?;
    let prefix = format!("daily-{day}-");
    if !force
        && let Some(existing) = backup_files(&directory)?.into_iter().find(|file| {
            file.path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                && validate_backup(&file.path).is_ok()
        })
    {
        rotate_backups(&directory)?;
        return Ok(existing.created_at_ms);
    }
    let backup_directory = directory.join("backups");
    fs::create_dir_all(&backup_directory)
        .map_err(|error| io_error("create_backup_directory", error))?;
    let id = uuid::Uuid::now_v7();
    let temporary = backup_directory.join(format!(".{id}.tmp"));
    let published = backup_directory.join(format!("{prefix}{id}.db"));
    let result = (|| {
        connection
            .backup(MAIN_DB, &temporary, None)
            .map_err(|error| io_error("backup_snapshot", error))?;
        validate_backup(&temporary)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error("sync_backup", error))?;
        fs::rename(&temporary, &published).map_err(|error| io_error("publish_backup", error))?;
        let _ = connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        rotate_backups(&directory)?;
        Ok(timestamp_ms(SystemTime::now()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 最新七个自然日和最近四个周各保留一份；所有备份最多保留 30 天。
fn rotate_backups(directory: &Path) -> Result<(), ApplicationError> {
    let cutoff = timestamp_ms(SystemTime::now()) - 30 * DAY_MS;
    let mut days = std::collections::BTreeSet::new();
    let mut weeks = std::collections::BTreeSet::new();
    for file in backup_files(directory)? {
        let date = chrono::DateTime::from_timestamp_millis(file.created_at_ms)
            .ok_or_else(|| io_error("rotate_backup", "备份日期无效"))?
            .with_timezone(&Local);
        let day = date.date_naive();
        let week = (date.iso_week().year(), date.iso_week().week());
        let daily = days.len() < 7 && days.insert(day);
        let weekly = weeks.len() < 4 && weeks.insert(week);
        let migration = file
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("pre-migration-"));
        if file.created_at_ms < cutoff || (!daily && !weekly && !migration) {
            fs::remove_file(file.path).map_err(|error| io_error("rotate_backups", error))?;
        }
    }
    Ok(())
}

/// 通过连接取得目录，避免把外部路径穿过单写者接口。
pub(super) fn data_directory(connection: &Connection) -> Result<PathBuf, ApplicationError> {
    connection
        .path()
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| io_error("database_directory", "没有可用的数据目录"))
}

fn timestamp_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn io_error(operation: &'static str, error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::storage(operation, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_check_does_not_treat_a_locked_database_as_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("health.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE fixture (value TEXT);")
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.busy_timeout(Duration::ZERO).unwrap();
        // 独占锁模拟检查期间的暂时占用，不应触发隔离或清空数据的恢复流程。
        writer.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        assert!(matches!(
            check_health(&reader),
            Err(ApplicationError::Storage { .. })
        ));
    }
}
