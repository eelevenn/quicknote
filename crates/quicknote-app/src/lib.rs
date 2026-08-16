//! QuickNote 的平台中立深应用模块。
//!
//! 调用方只需学习命令、快照和可观察错误；SQLite schema、迁移与单写者
//! 都留在模块实现内部。`PlatformServices` seam 与生产/测试 adapter 分离，
//! 避免 Win32 类型进入共享核心。

mod error;
mod interface;
pub mod platform;
mod storage;

pub use error::ApplicationError;
pub use interface::{
    ApplicationConfig, ApplicationSnapshot, Command, CommandResult, SchemaIdentity,
};

use storage::StorageWriter;

/// 供 Slint UI、平台壳和自动化共同使用的应用模块接口。
pub struct Application {
    writer: StorageWriter,
}

impl Application {
    /// 打开或创建数据库，并在返回前完成所有前向迁移。
    pub fn open(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        Self::open_with_fault(config, storage::MigrationFault::None)
    }

    /// 串行执行领域命令并等待事务提交结果。
    pub fn execute(&self, command: Command) -> Result<CommandResult, ApplicationError> {
        self.writer.execute(command)
    }

    /// 从同一个 SQLite 单写者读取一致的应用快照。
    pub fn snapshot(&self) -> Result<ApplicationSnapshot, ApplicationError> {
        self.writer.snapshot()
    }

    fn open_with_fault(
        config: ApplicationConfig,
        fault: storage::MigrationFault,
    ) -> Result<Self, ApplicationError> {
        let connection = storage::open_database(&config, fault)?;
        Ok(Self {
            writer: StorageWriter::start(connection)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Application, ApplicationConfig, ApplicationError};
    use crate::storage::{APPLICATION_ID, MigrationFault, SUPPORTED_SCHEMA_VERSION};
    use rusqlite::Connection;
    use tempfile::TempDir;

    #[test]
    fn future_schema_is_rejected_without_modification() {
        let directory = TempDir::new().expect("创建临时目录");
        let database_path = directory.path().join("quicknote.db");
        let connection = Connection::open(&database_path).expect("创建未来版本数据库");
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .expect("写入应用身份");
        connection
            .pragma_update(None, "user_version", SUPPORTED_SCHEMA_VERSION + 1)
            .expect("写入未来 schema 版本");
        connection
            .execute_batch("CREATE TABLE future_marker(value TEXT NOT NULL); INSERT INTO future_marker VALUES ('kept');")
            .expect("写入未来版本标记");
        drop(connection);

        let error = match Application::open(ApplicationConfig::new(directory.path())) {
            Ok(_) => panic!("未来 schema 不应被旧客户端打开"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ApplicationError::UnsupportedSchema {
                found,
                supported
            } if found == SUPPORTED_SCHEMA_VERSION + 1 && supported == SUPPORTED_SCHEMA_VERSION
        ));

        let connection = Connection::open(database_path).expect("重新检查未来版本数据库");
        let marker: String = connection
            .query_row("SELECT value FROM future_marker", [], |row| row.get(0))
            .expect("未来版本数据应保持原样");
        assert_eq!(marker, "kept");
    }

    #[test]
    fn failed_migration_rolls_back_and_keeps_verified_backup() {
        let directory = TempDir::new().expect("创建临时目录");
        let database_path = directory.path().join("quicknote.db");
        let connection = Connection::open(&database_path).expect("创建旧版本数据库");
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .expect("写入应用身份");
        connection
            .execute_batch("CREATE TABLE legacy_marker(value TEXT NOT NULL); INSERT INTO legacy_marker VALUES ('original');")
            .expect("写入旧版本数据");
        drop(connection);

        let error = match Application::open_with_fault(
            ApplicationConfig::new(directory.path()),
            MigrationFault::AfterSchema,
        ) {
            Ok(_) => panic!("注入失败后不应完成迁移"),
            Err(error) => error,
        };
        let backup_path = match error {
            ApplicationError::Migration {
                from: 0,
                to,
                backup_path: Some(path),
                ..
            } if to == SUPPORTED_SCHEMA_VERSION => path,
            other => panic!("应返回带备份路径的迁移错误，实际为 {other:?}"),
        };

        let original = Connection::open(database_path).expect("重新打开原数据库");
        let original_version: i32 = original
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("读取原 schema 版本");
        let original_value: String = original
            .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
            .expect("读取原数据");
        assert_eq!(original_version, 0);
        assert_eq!(original_value, "original");

        let backup = Connection::open(backup_path).expect("打开迁移前备份");
        let quick_check: String = backup
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .expect("校验迁移前备份");
        let backup_value: String = backup
            .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
            .expect("读取备份数据");
        assert_eq!(quick_check, "ok");
        assert_eq!(backup_value, "original");
    }
}
