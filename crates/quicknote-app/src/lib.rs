//! QuickNote 的平台中立深应用模块。
//!
//! 调用方只需学习命令、快照和可观察错误；SQLite schema、迁移与单写者
//! 都留在模块实现内部。`PlatformServices` seam 与生产/测试 adapter 分离，
//! 避免 Win32 类型进入共享核心。

mod editing;
mod error;
mod interface;
pub mod platform;
mod storage;

pub use error::ApplicationError;
pub use interface::{
    ApplicationConfig, ApplicationSnapshot, Command, CommandResult, EditingSnapshot, EditorIntent,
    NoteSummary, SaveState, SchemaIdentity,
};

use platform::{GlobalShortcut, PlatformCommand, PlatformServices};
use storage::{StorageClient, StorageFaultPlan, StorageWriter};

/// 供 Slint UI、平台壳和自动化共同使用的应用模块接口。
pub struct Application {
    // 字段顺序确保编辑协调器先停止，再关闭 SQLite 单写者。
    editor: editing::Editor,
    storage: StorageClient,
    _writer: StorageWriter,
}

impl Application {
    /// 打开或创建数据库，并在返回前完成所有前向迁移。
    pub fn open(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        Self::open_with_fault(config, storage::MigrationFault::None)
    }

    /// 串行执行领域命令并等待事务提交结果。
    pub fn execute(&self, command: Command) -> Result<CommandResult, ApplicationError> {
        self.storage.execute(command)
    }

    /// 从同一个 SQLite 单写者读取一致的应用快照。
    pub fn snapshot(&self) -> Result<ApplicationSnapshot, ApplicationError> {
        self.storage.snapshot()
    }

    /// 读取主页与快速记录共同拥有的编辑状态。
    pub fn editing_snapshot(&self) -> Result<EditingSnapshot, ApplicationError> {
        self.editor.snapshot()
    }

    /// 提交编辑意图；自动保存计时、合并和刷新顺序由模块内部保证。
    pub fn edit(&self, intent: EditorIntent) -> Result<EditingSnapshot, ApplicationError> {
        self.editor.apply(intent)
    }

    /// 启动时注册已经持久化的快捷键；冲突不会改写设置。
    pub fn install_global_shortcut(
        &self,
        platform: &dyn PlatformServices,
    ) -> Result<GlobalShortcut, ApplicationError> {
        let shortcut = self.snapshot()?.global_shortcut;
        platform
            .apply(PlatformCommand::SetGlobalShortcut { shortcut })
            .map_err(ApplicationError::platform)?;
        Ok(shortcut)
    }

    /// 先让平台原子替换快捷键，再持久化；任一步失败都尽力恢复旧组合。
    pub fn configure_global_shortcut(
        &self,
        platform: &dyn PlatformServices,
        value: &str,
    ) -> Result<GlobalShortcut, ApplicationError> {
        let shortcut =
            GlobalShortcut::parse(value).map_err(|error| ApplicationError::InvalidCommand {
                message: error.to_string(),
            })?;
        let old_shortcut = self.snapshot()?.global_shortcut;
        platform
            .apply(PlatformCommand::SetGlobalShortcut { shortcut })
            .map_err(ApplicationError::platform)?;

        if let Err(storage_error) = self.storage.persist_global_shortcut(shortcut) {
            if let Err(rollback_error) = platform.apply(PlatformCommand::SetGlobalShortcut {
                shortcut: old_shortcut,
            }) {
                return Err(ApplicationError::Platform {
                    operation: "rollback_global_shortcut",
                    message: format!(
                        "设置持久化失败：{storage_error}；旧快捷键恢复失败：{rollback_error}"
                    ),
                });
            }
            return Err(storage_error);
        }
        Ok(shortcut)
    }

    fn open_with_fault(
        config: ApplicationConfig,
        fault: storage::MigrationFault,
    ) -> Result<Self, ApplicationError> {
        Self::open_with_faults(config, fault, StorageFaultPlan::default())
    }

    fn open_with_faults(
        config: ApplicationConfig,
        migration_fault: storage::MigrationFault,
        storage_faults: StorageFaultPlan,
    ) -> Result<Self, ApplicationError> {
        let connection = storage::open_database(&config, migration_fault)?;
        let writer = StorageWriter::start(connection, storage_faults)?;
        let storage = writer.client();
        let editor = editing::Editor::start(storage.clone())?;
        Ok(Self {
            editor,
            storage,
            _writer: writer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Application, ApplicationConfig, ApplicationError, EditorIntent, SaveState};
    use crate::platform::test_support::TestPlatformServices;
    use crate::platform::{GlobalShortcut, PlatformCommand};
    use crate::storage::{
        APPLICATION_ID, MigrationFault, SUPPORTED_SCHEMA_VERSION, StorageFaultPlan,
    };
    use rusqlite::Connection;
    use std::thread;
    use std::time::{Duration, Instant};
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

    #[test]
    fn failed_first_save_rolls_back_identity_body_and_current_pointer() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open_with_faults(
            ApplicationConfig::new(directory.path()),
            MigrationFault::None,
            StorageFaultPlan::fail_save_attempt(1),
        )
        .expect("打开带故障计划的应用");

        app.edit(EditorIntent::ReplaceBody("首次正文".to_owned()))
            .expect("更新内存正文");
        let error = app
            .edit(EditorIntent::Flush)
            .expect_err("提交前故障必须让刷新失败");
        assert!(matches!(error, ApplicationError::Storage { .. }));
        let editing = app.editing_snapshot().expect("读取失败后的内存正文");
        assert_eq!(editing.note_id, None);
        assert_eq!(editing.body, "首次正文");
        assert!(matches!(editing.save_state, SaveState::Failed { .. }));
        let persisted = app.snapshot().expect("读取失败后的数据库事实");
        assert_eq!(persisted.active_note_count, 0);
        assert_eq!(persisted.current_note_id, None);

        app.edit(EditorIntent::RetrySave).expect("请求重试");
        app.edit(EditorIntent::Flush).expect("重试应成功");
        assert_eq!(app.snapshot().expect("读取重试结果").active_note_count, 1);
    }

    #[test]
    fn failed_flush_aborts_switch_and_preserves_old_editor_and_pointer() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open_with_faults(
            ApplicationConfig::new(directory.path()),
            MigrationFault::None,
            StorageFaultPlan::fail_save_attempt(3),
        )
        .expect("打开带第三次保存故障的应用");

        app.edit(EditorIntent::ReplaceBody("便签 A".to_owned()))
            .expect("编辑 A");
        let note_a = app
            .edit(EditorIntent::Flush)
            .expect("保存 A")
            .note_id
            .expect("A 已有身份");
        app.edit(EditorIntent::NewBlankDraft).expect("创建空白草稿");
        app.edit(EditorIntent::ReplaceBody("便签 B".to_owned()))
            .expect("编辑 B");
        let note_b = app
            .edit(EditorIntent::Flush)
            .expect("保存 B")
            .note_id
            .expect("B 已有身份");
        app.edit(EditorIntent::SwitchCurrent(note_a))
            .expect("切回 A");
        app.edit(EditorIntent::ReplaceBody("便签 A 的未保存修订".to_owned()))
            .expect("修改 A");

        app.edit(EditorIntent::SwitchCurrent(note_b))
            .expect_err("旧编辑保存失败必须中止切换");
        let editing = app.editing_snapshot().expect("读取中止后的编辑状态");
        assert_eq!(editing.note_id, Some(note_a));
        assert_eq!(editing.body, "便签 A 的未保存修订");
        assert_eq!(
            app.snapshot().expect("读取当前指针").current_note_id,
            Some(note_a)
        );
    }

    #[test]
    fn failed_current_pointer_transaction_keeps_previous_current_note() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open_with_faults(
            ApplicationConfig::new(directory.path()),
            MigrationFault::None,
            StorageFaultPlan::fail_switch_attempt(1),
        )
        .expect("打开带切换故障的应用");
        app.edit(EditorIntent::ReplaceBody("便签 A".to_owned()))
            .expect("编辑 A");
        let note_a = app
            .edit(EditorIntent::Flush)
            .expect("保存 A")
            .note_id
            .expect("A 已有身份");
        app.edit(EditorIntent::NewBlankDraft).expect("创建空白草稿");
        app.edit(EditorIntent::ReplaceBody("便签 B".to_owned()))
            .expect("编辑 B");
        let note_b = app
            .edit(EditorIntent::Flush)
            .expect("保存 B")
            .note_id
            .expect("B 已有身份");

        app.edit(EditorIntent::SwitchCurrent(note_a))
            .expect_err("切换事务故障必须回滚");
        assert_eq!(
            app.snapshot().expect("读取当前指针").current_note_id,
            Some(note_b)
        );
        assert_eq!(
            app.editing_snapshot().expect("读取编辑状态").note_id,
            Some(note_b)
        );
    }

    #[test]
    fn shortcut_conflict_keeps_old_valid_registration_and_setting() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
        let platform = TestPlatformServices::new(directory.path());
        app.install_global_shortcut(&platform)
            .expect("注册默认快捷键");
        platform
            .fail_next_apply("组合已被占用")
            .expect("注入下一次平台冲突");

        app.configure_global_shortcut(&platform, "Ctrl+Shift+K")
            .expect_err("冲突必须返回可恢复错误");
        assert_eq!(
            app.snapshot().expect("读取冲突后的设置").global_shortcut,
            GlobalShortcut::default()
        );
        assert_eq!(
            platform.recorded_commands().expect("读取平台注册记录"),
            vec![PlatformCommand::SetGlobalShortcut {
                shortcut: GlobalShortcut::default()
            }]
        );

        let updated = app
            .configure_global_shortcut(&platform, "Ctrl+Shift+K")
            .expect("重试有效组合");
        assert_eq!(updated.to_string(), "Ctrl+Shift+K");
        assert_eq!(
            app.snapshot().expect("读取新快捷键").global_shortcut,
            updated
        );
    }

    #[test]
    fn old_save_receipt_never_marks_a_newer_memory_revision_as_saved() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open_with_faults(
            ApplicationConfig::new(directory.path()),
            MigrationFault::None,
            StorageFaultPlan::with_save_delay(Duration::from_millis(200)),
        )
        .expect("打开带保存延迟的应用");
        app.edit(EditorIntent::ReplaceBody("版本 A".to_owned()))
            .expect("编辑版本 A");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(
            app.editing_snapshot().expect("等待版本 A 在途").save_state,
            SaveState::Saving
        ) {
            assert!(Instant::now() < deadline, "版本 A 应进入在途状态");
            thread::sleep(Duration::from_millis(5));
        }
        app.edit(EditorIntent::ReplaceBody("版本 B".to_owned()))
            .expect("在 A 在途时编辑 B");

        let deadline = Instant::now() + Duration::from_secs(2);
        let after_old_receipt = loop {
            let snapshot = app.editing_snapshot().expect("等待旧保存回执");
            if snapshot.revision == 2 && snapshot.saved_revision == 1 {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "应观察到 A 的旧回执");
            thread::sleep(Duration::from_millis(5));
        };
        assert!(!matches!(after_old_receipt.save_state, SaveState::Saved));
        app.edit(EditorIntent::Flush).expect("刷新最新版本 B");
        let final_state = app.editing_snapshot().expect("读取最终保存状态");
        assert_eq!(final_state.body, "版本 B");
        assert_eq!(final_state.revision, final_state.saved_revision);
        assert!(matches!(final_state.save_state, SaveState::Saved));
    }
}
