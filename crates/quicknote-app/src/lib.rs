//! QuickNote 的平台中立深应用模块。
//!
//! 调用方只需学习命令、快照和可观察错误；SQLite schema、迁移与单写者
//! 都留在模块实现内部。`PlatformServices` seam 与生产/测试 adapter 分离，
//! 避免 Win32 类型进入共享核心。

mod editing;
mod error;
mod interface;
mod markdown;
pub mod platform;
mod storage;
pub mod transcription;

pub use error::ApplicationError;
pub use interface::{
    ApplicationConfig, ApplicationSnapshot, Command, CommandResult, EditingSnapshot, EditorIntent,
    ExportBundle, ExportReminder, ExportSettings, LibrarySnapshot, MarkdownLink, MarkdownPreview,
    NoteAction, NoteDocument, NoteLifecycle, NoteSummary, NoteTiming, ReminderActivation,
    ReminderActivationAction, ReminderActivationOutcome, ReminderCoordination,
    ReminderCoordinationReason, ReminderSnapshot, ReminderStatus, SaveState, SchemaIdentity,
    SearchResult,
};

use platform::{GlobalShortcut, PlatformCommand, PlatformServices};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use storage::{StorageClient, StorageFaultPlan, StorageWriter};
use uuid::Uuid;

/// 供 Slint UI、平台壳和自动化共同使用的应用模块接口。
pub struct Application {
    // 字段顺序确保编辑协调器先停止，再关闭 SQLite 单写者。
    editor: editing::Editor,
    storage: StorageClient,
    reminder_focus: Mutex<ReminderFocusState>,
    _writer: StorageWriter,
}

#[derive(Default)]
struct ReminderFocusState {
    focused_note_id: Option<Uuid>,
    observed_at_ms: Option<i64>,
    last_platform_reconcile_ms: Option<i64>,
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

    /// 读取活跃、归档和回收站的完整导航摘要。
    pub fn library_snapshot(&self) -> Result<LibrarySnapshot, ApplicationError> {
        self.storage.library_snapshot()
    }

    /// 读取一张未永久清除便签；归档与回收站正文始终只读。
    pub fn note(&self, note_id: Uuid) -> Result<NoteDocument, ApplicationError> {
        self.storage.read_note(note_id)
    }

    /// 读取一张活跃便签相互独立的截止时间与单一提醒。
    pub fn note_timing(&self, note_id: Uuid) -> Result<NoteTiming, ApplicationError> {
        self.storage.read_note_timing(note_id)
    }

    /// 更新截止时间；首次设置未来截止且提醒为空时自动创建同一时刻提醒。
    pub fn set_due_at(
        &self,
        platform: &dyn PlatformServices,
        note_id: Uuid,
        due_at_ms: Option<i64>,
    ) -> Result<NoteTiming, ApplicationError> {
        self.editor.apply(EditorIntent::Flush)?;
        self.storage.set_due_at(note_id, due_at_ms, now_ms())?;
        let coordination = self.project_reminder_outbox(platform, true)?;
        self.timing_with_coordination(note_id, coordination)
    }

    /// 创建、替换或清除一张活跃便签的单一未来提醒。
    pub fn set_reminder(
        &self,
        platform: &dyn PlatformServices,
        note_id: Uuid,
        scheduled_at_ms: Option<i64>,
    ) -> Result<NoteTiming, ApplicationError> {
        self.editor.apply(EditorIntent::Flush)?;
        self.storage
            .set_reminder(note_id, scheduled_at_ms, now_ms())?;
        let coordination = self.project_reminder_outbox(platform, true)?;
        self.timing_with_coordination(note_id, coordination)
    }

    /// 用户主动打开活跃便签时，仅响应已经到点的提醒。
    pub fn respond_to_due_reminder(
        &self,
        platform: &dyn PlatformServices,
        note_id: Uuid,
    ) -> Result<bool, ApplicationError> {
        let responded = self.storage.respond_to_due_reminder(note_id, now_ms())?;
        if responded {
            let _ = self.project_reminder_outbox(platform, true)?;
        }
        Ok(responded)
    }

    /// 幂等处理通知打开或稍后提醒动作；平台失败不会回滚领域事实。
    pub fn handle_reminder_activation(
        &self,
        platform: &dyn PlatformServices,
        activation: ReminderActivation,
    ) -> Result<ReminderActivationOutcome, ApplicationError> {
        if activation.action == ReminderActivationAction::Open {
            self.editor.apply(EditorIntent::Flush)?;
        }
        let outcome = self
            .storage
            .handle_reminder_activation(activation, now_ms())?;
        let _ = self.project_reminder_outbox(platform, true)?;
        if matches!(outcome, ReminderActivationOutcome::Opened { .. }) {
            self.editor.apply(EditorIntent::OpenCurrent)?;
        }
        Ok(outcome)
    }

    /// 在启动、持续运行和恢复阶段收敛到期状态、outbox 与 Windows 计划。
    pub fn coordinate_reminders(
        &self,
        platform: &dyn PlatformServices,
        focused_note_id: Option<Uuid>,
        reason: ReminderCoordinationReason,
    ) -> Result<ReminderCoordination, ApplicationError> {
        let timestamp = now_ms();
        let (focused_since_ms, should_reconcile) = {
            let mut state = self.reminder_focus.lock().map_err(|error| {
                ApplicationError::WriterUnavailable {
                    message: format!("提醒焦点状态不可用：{error}"),
                }
            })?;
            let focused_since_ms = if reason == ReminderCoordinationReason::Continuous
                && state.focused_note_id == focused_note_id
            {
                state.observed_at_ms
            } else {
                None
            };
            let should_reconcile = reason != ReminderCoordinationReason::Continuous
                || state
                    .last_platform_reconcile_ms
                    .is_none_or(|last| timestamp.saturating_sub(last) >= 60_000);
            state.focused_note_id = focused_note_id;
            state.observed_at_ms = Some(timestamp);
            if should_reconcile {
                state.last_platform_reconcile_ms = Some(timestamp);
            }
            (focused_since_ms, should_reconcile)
        };
        self.storage.advance_due_reminders(
            timestamp,
            focused_note_id,
            focused_since_ms,
            reason != ReminderCoordinationReason::Continuous,
        )?;
        self.project_reminder_outbox(platform, should_reconcile)
    }

    /// 搜索活跃和归档正文，使用大小写不敏感的文字子串合同。
    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>, ApplicationError> {
        self.storage.search(query)
    }

    /// 读取主页与快速记录共同拥有的编辑状态。
    pub fn editing_snapshot(&self) -> Result<EditingSnapshot, ApplicationError> {
        self.editor.snapshot()
    }

    /// 提交编辑意图；自动保存计时、合并和刷新顺序由模块内部保证。
    pub fn edit(&self, intent: EditorIntent) -> Result<EditingSnapshot, ApplicationError> {
        self.editor.apply(intent)
    }

    /// 刷新全部编辑后执行严格的便签生命周期转换。
    pub fn apply_note_action(
        &self,
        action: NoteAction,
    ) -> Result<EditingSnapshot, ApplicationError> {
        self.editor.apply(EditorIntent::Flush)?;
        self.storage.apply_note_action(action)?;
        self.editor.apply(EditorIntent::OpenCurrent)
    }

    /// 立即执行一次 30 天回收站维护；后台单写者也会每 24 小时执行。
    pub fn maintain_trash(&self) -> Result<u64, ApplicationError> {
        let deleted = self.storage.maintain_trash()?;
        self.editor.apply(EditorIntent::OpenCurrent)?;
        Ok(deleted)
    }

    /// 把 Markdown 原文解析为安全的纯文本预览和显式链接列表。
    pub fn preview_markdown(source: &str) -> MarkdownPreview {
        markdown::render_preview(source)
    }

    /// 刷新编辑后导出版本化、无损且不含内部投影状态的 JSON。
    pub fn export_json(
        &self,
        platform: &dyn PlatformServices,
        target: &Path,
    ) -> Result<(), ApplicationError> {
        self.editor.apply(EditorIntent::Flush)?;
        let bundle = self.storage.export_bundle()?;
        let mut bytes = serde_json::to_vec_pretty(&bundle)
            .map_err(|error| ApplicationError::storage("encode_json_export", error))?;
        bytes.push(b'\n');
        platform
            .write_file_atomically(target, &bytes)
            .map_err(ApplicationError::platform)
    }

    /// 刷新编辑后把一张便签以 YAML front matter 加原始正文导出。
    pub fn export_markdown(
        &self,
        platform: &dyn PlatformServices,
        note_id: Uuid,
        target: &Path,
    ) -> Result<(), ApplicationError> {
        self.editor.apply(EditorIntent::Flush)?;
        let note = self.storage.read_note(note_id)?;
        let bytes = markdown::markdown_export(&note);
        platform
            .write_file_atomically(target, bytes.as_bytes())
            .map_err(ApplicationError::platform)
    }

    /// 只有用户明确点击预览链接时才请求平台打开 http(s) 地址。
    pub fn open_external_link(
        &self,
        platform: &dyn PlatformServices,
        url: &str,
    ) -> Result<(), ApplicationError> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(ApplicationError::InvalidCommand {
                message: "只允许明确打开 http 或 https 外部链接".to_owned(),
            });
        }
        platform
            .apply(PlatformCommand::OpenExternalLink {
                url: url.to_owned(),
            })
            .map_err(ApplicationError::platform)
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

    /// 先更新系统自启动，再持久化设置；失败时恢复旧平台状态。
    pub fn configure_startup(
        &self,
        platform: &dyn PlatformServices,
        enabled: bool,
    ) -> Result<(), ApplicationError> {
        let old_enabled = self.snapshot()?.startup_enabled;
        platform
            .apply(PlatformCommand::SetStartupEnabled { enabled })
            .map_err(ApplicationError::platform)?;
        if let Err(storage_error) = self.storage.persist_startup_enabled(enabled) {
            if let Err(rollback_error) = platform.apply(PlatformCommand::SetStartupEnabled {
                enabled: old_enabled,
            }) {
                return Err(ApplicationError::Platform {
                    operation: "rollback_startup_setting",
                    message: format!(
                        "设置持久化失败：{storage_error}；旧自启动状态恢复失败：{rollback_error}"
                    ),
                });
            }
            return Err(storage_error);
        }
        Ok(())
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
        // 启动时先清理绝对保留期已满的回收站便签，再建立编辑视图。
        storage.maintain_trash()?;
        let editor = editing::Editor::start(storage.clone())?;
        Ok(Self {
            editor,
            storage,
            reminder_focus: Mutex::new(ReminderFocusState::default()),
            _writer: writer,
        })
    }

    fn project_reminder_outbox(
        &self,
        platform: &dyn PlatformServices,
        reconcile: bool,
    ) -> Result<ReminderCoordination, ApplicationError> {
        let timestamp = now_ms();
        let mut platform_error = None;
        if reconcile {
            match platform.scheduled_notifications() {
                Ok(scheduled) => self
                    .storage
                    .reconcile_reminder_projections(scheduled, timestamp)?,
                Err(error) => platform_error = Some(error.to_string()),
            }
        }
        for projection in self.storage.ready_reminder_projections(timestamp)? {
            match platform.apply(projection.command) {
                Ok(()) => self
                    .storage
                    .complete_reminder_projection(projection.outbox_id)?,
                Err(error) => {
                    let message = error.to_string();
                    self.storage.fail_reminder_projection(
                        projection.outbox_id,
                        timestamp,
                        message.clone(),
                    )?;
                    platform_error = Some(message);
                    break;
                }
            }
        }
        Ok(ReminderCoordination {
            pending_projection_count: self.storage.pending_reminder_projection_count()?,
            platform_error,
        })
    }

    fn timing_with_coordination(
        &self,
        note_id: Uuid,
        coordination: ReminderCoordination,
    ) -> Result<NoteTiming, ApplicationError> {
        let mut timing = self.storage.read_note_timing(note_id)?;
        if coordination.pending_projection_count > 0 {
            timing.platform_sync_pending = true;
            timing.platform_sync_error = coordination.platform_error.clone();
            if let Some(reminder) = timing.reminder.as_mut() {
                reminder.platform_sync_pending = true;
                reminder.platform_sync_error = coordination.platform_error;
            }
        }
        Ok(timing)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        Application, ApplicationConfig, ApplicationError, EditorIntent, NoteAction, SaveState,
    };
    use crate::platform::test_support::TestPlatformServices;
    use crate::platform::{GlobalShortcut, PlatformCommand};
    use crate::storage::{
        APPLICATION_ID, MigrationFault, SUPPORTED_SCHEMA_VERSION, StorageFaultPlan,
    };
    use rusqlite::Connection;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

    #[test]
    fn startup_maintenance_purges_only_notes_at_the_absolute_thirty_day_boundary() {
        let directory = TempDir::new().expect("创建临时目录");
        let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开应用");
        app.edit(EditorIntent::ReplaceBody("到期便签".to_owned()))
            .expect("编辑到期便签");
        let expired = app
            .edit(EditorIntent::Flush)
            .expect("保存到期便签")
            .note_id
            .expect("到期便签身份");
        app.apply_note_action(NoteAction::Archive(expired))
            .expect("归档到期便签");
        app.apply_note_action(NoteAction::MoveToTrash(expired))
            .expect("移入回收站");
        app.edit(EditorIntent::NewBlankDraft).expect("新建保留便签");
        app.edit(EditorIntent::ReplaceBody("尚未到期".to_owned()))
            .expect("编辑保留便签");
        let retained = app
            .edit(EditorIntent::Flush)
            .expect("保存保留便签")
            .note_id
            .expect("保留便签身份");
        app.apply_note_action(NoteAction::Archive(retained))
            .expect("归档保留便签");
        app.apply_note_action(NoteAction::MoveToTrash(retained))
            .expect("保留便签进入回收站");
        drop(app);

        let now_ms: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间有效")
            .as_millis()
            .try_into()
            .expect("时间在 i64 范围");
        let exact_boundary = now_ms - 30 * 24 * 60 * 60 * 1_000;
        let connection =
            Connection::open(directory.path().join("quicknote.db")).expect("打开维护测试数据库");
        connection
            .execute(
                "UPDATE notes SET trashed_at_ms = ?1 WHERE id = ?2",
                rusqlite::params![exact_boundary - 1_000, expired.as_bytes().as_slice()],
            )
            .expect("设置到期时间");
        connection
            .execute(
                "UPDATE notes SET trashed_at_ms = ?1 WHERE id = ?2",
                rusqlite::params![exact_boundary + 60_000, retained.as_bytes().as_slice()],
            )
            .expect("设置未到期时间");
        drop(connection);

        let reopened =
            Application::open(ApplicationConfig::new(directory.path())).expect("启动并维护");
        assert!(reopened.note(expired).is_err());
        assert_eq!(
            reopened
                .snapshot()
                .expect("读取维护结果")
                .trashed_note_count,
            1
        );
        assert!(reopened.note(retained).is_ok());
    }
}
