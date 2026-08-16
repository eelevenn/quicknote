//! SQLite 单写者与应用命令实现。

mod migrations;

use crate::platform::GlobalShortcut;
use crate::{
    ApplicationConfig, ApplicationError, ApplicationSnapshot, Command, CommandResult, NoteSummary,
    SchemaIdentity,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub(crate) use migrations::MigrationFault;
#[cfg(test)]
pub(crate) use migrations::{APPLICATION_ID, SUPPORTED_SCHEMA_VERSION};

/// 打开连接并在启动 UI 前完成身份检查和迁移。
pub(crate) fn open_database(
    config: &ApplicationConfig,
    fault: MigrationFault,
) -> Result<Connection, ApplicationError> {
    migrations::open_database(config.data_directory(), fault)
}

/// 编辑协调器持有的存储视图，不向 UI 暴露表结构。
#[derive(Clone, Debug)]
pub(crate) struct EditingView {
    pub(crate) document: StoredDocument,
    pub(crate) active_notes: Vec<NoteSummary>,
}

/// 当前编辑目标在 SQLite 中的投影。
#[derive(Clone, Debug)]
pub(crate) struct StoredDocument {
    pub(crate) note_id: Option<Uuid>,
    pub(crate) body: String,
    pub(crate) title: String,
    pub(crate) revision: u64,
}

/// 一个已经被自动保存协调器合并后的最新保存请求。
#[derive(Clone, Debug)]
pub(crate) struct SaveRequest {
    pub(crate) persisted_note_id: Option<Uuid>,
    pub(crate) candidate_note_id: Uuid,
    pub(crate) body: String,
    pub(crate) revision: u64,
}

enum StorageRequest {
    Execute {
        command: Command,
        respond_to: Sender<Result<CommandResult, ApplicationError>>,
    },
    Snapshot {
        respond_to: Sender<Result<ApplicationSnapshot, ApplicationError>>,
    },
    PersistGlobalShortcut {
        shortcut: GlobalShortcut,
        respond_to: Sender<Result<(), ApplicationError>>,
    },
    LoadEditingView {
        respond_to: Sender<Result<EditingView, ApplicationError>>,
    },
    Save {
        request: SaveRequest,
        respond_to: Sender<Result<EditingView, ApplicationError>>,
    },
    SwitchCurrent {
        note_id: Uuid,
        respond_to: Sender<Result<EditingView, ApplicationError>>,
    },
    Shutdown,
}

/// 只用于模块内确定性验收的事务故障计划。
#[derive(Clone, Debug, Default)]
pub(crate) struct StorageFaultPlan {
    save_attempts_after_write: BTreeSet<u64>,
    switch_attempts_after_write: BTreeSet<u64>,
    save_delay: Duration,
}

#[cfg(test)]
impl StorageFaultPlan {
    /// 在指定的保存尝试写入事务后、提交前失败。
    pub(crate) fn fail_save_attempt(attempt: u64) -> Self {
        Self {
            save_attempts_after_write: BTreeSet::from([attempt]),
            switch_attempts_after_write: BTreeSet::new(),
            save_delay: Duration::ZERO,
        }
    }

    /// 在指定的当前便签切换写入后、提交前失败。
    pub(crate) fn fail_switch_attempt(attempt: u64) -> Self {
        Self {
            save_attempts_after_write: BTreeSet::new(),
            switch_attempts_after_write: BTreeSet::from([attempt]),
            save_delay: Duration::ZERO,
        }
    }

    /// 延迟每次保存响应，用于确定性验证旧回执不能越级标记新修订。
    pub(crate) fn with_save_delay(save_delay: Duration) -> Self {
        Self {
            save_attempts_after_write: BTreeSet::new(),
            switch_attempts_after_write: BTreeSet::new(),
            save_delay,
        }
    }
}

struct StorageFaultRuntime {
    plan: StorageFaultPlan,
    save_attempts: u64,
    switch_attempts: u64,
}

impl StorageFaultRuntime {
    fn new(plan: StorageFaultPlan) -> Self {
        Self {
            plan,
            save_attempts: 0,
            switch_attempts: 0,
        }
    }

    fn fail_next_save_after_write(&mut self) -> bool {
        self.save_attempts += 1;
        self.plan
            .save_attempts_after_write
            .contains(&self.save_attempts)
    }

    fn fail_next_switch_after_write(&mut self) -> bool {
        self.switch_attempts += 1;
        self.plan
            .switch_attempts_after_write
            .contains(&self.switch_attempts)
    }

    fn save_delay(&self) -> Duration {
        self.plan.save_delay
    }
}

/// 可克隆客户端只提交请求；SQLite 连接仍由唯一 worker 拥有。
#[derive(Clone)]
pub(crate) struct StorageClient {
    sender: Sender<StorageRequest>,
}

impl StorageClient {
    pub(crate) fn execute(&self, command: Command) -> Result<CommandResult, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::Execute {
                command,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn snapshot(&self) -> Result<ApplicationSnapshot, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::Snapshot { respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn persist_global_shortcut(
        &self,
        shortcut: GlobalShortcut,
    ) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::PersistGlobalShortcut {
                shortcut,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn load_editing_view(&self) -> Result<EditingView, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::LoadEditingView { respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn save(&self, request: SaveRequest) -> Result<EditingView, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::Save {
                request,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn switch_current(&self, note_id: Uuid) -> Result<EditingView, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::SwitchCurrent {
                note_id,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }
}

/// 独占 SQLite 连接并串行处理所有读写请求。
pub(crate) struct StorageWriter {
    sender: Sender<StorageRequest>,
    worker: Option<JoinHandle<()>>,
}

impl StorageWriter {
    /// 把已迁移连接移动到唯一后台线程。
    pub(crate) fn start(
        connection: Connection,
        faults: StorageFaultPlan,
    ) -> Result<Self, ApplicationError> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("quicknote-sqlite-writer".to_owned())
            .spawn(move || run_writer(connection, receiver, faults))
            .map_err(|error| ApplicationError::WriterUnavailable {
                message: error.to_string(),
            })?;
        Ok(Self {
            sender,
            worker: Some(worker),
        })
    }

    /// 创建供应用命令和编辑协调器共享的轻量客户端。
    pub(crate) fn client(&self) -> StorageClient {
        StorageClient {
            sender: self.sender.clone(),
        }
    }
}

impl Drop for StorageWriter {
    fn drop(&mut self) {
        // Application 先回收编辑协调器，再让独占线程处理完已提交的请求。
        let _ = self.sender.send(StorageRequest::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn writer_disconnected(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::WriterUnavailable {
        message: error.to_string(),
    }
}

fn run_writer(
    mut connection: Connection,
    receiver: Receiver<StorageRequest>,
    faults: StorageFaultPlan,
) {
    let mut faults = StorageFaultRuntime::new(faults);
    while let Ok(request) = receiver.recv() {
        match request {
            StorageRequest::Execute {
                command,
                respond_to,
            } => {
                let _ = respond_to.send(execute_command(&mut connection, command));
            }
            StorageRequest::Snapshot { respond_to } => {
                let _ = respond_to.send(read_snapshot(&connection));
            }
            StorageRequest::PersistGlobalShortcut {
                shortcut,
                respond_to,
            } => {
                let result =
                    write_setting(&mut connection, "global_shortcut", &shortcut.to_string())
                        .map(|_| ());
                let _ = respond_to.send(result);
            }
            StorageRequest::LoadEditingView { respond_to } => {
                let _ = respond_to.send(read_editing_view(&connection));
            }
            StorageRequest::Save {
                request,
                respond_to,
            } => {
                let fail_after_write = faults.fail_next_save_after_write();
                let save_delay = faults.save_delay();
                if !save_delay.is_zero() {
                    thread::sleep(save_delay);
                }
                let _ = respond_to.send(save_document(&mut connection, request, fail_after_write));
            }
            StorageRequest::SwitchCurrent {
                note_id,
                respond_to,
            } => {
                let fail_after_write = faults.fail_next_switch_after_write();
                let _ = respond_to.send(switch_current_note(
                    &mut connection,
                    note_id,
                    fail_after_write,
                ));
            }
            StorageRequest::Shutdown => break,
        }
    }
    // 正常退出收敛 WAL，失败只影响维护状态，不破坏已提交事务。
    let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

fn execute_command(
    connection: &mut Connection,
    command: Command,
) -> Result<CommandResult, ApplicationError> {
    match command {
        Command::SetDefaultSnoozeMinutes { minutes } => {
            const ALLOWED: [u16; 5] = [5, 10, 15, 30, 60];
            if !ALLOWED.contains(&minutes) {
                return Err(ApplicationError::InvalidCommand {
                    message: format!("稍后提醒时长 {minutes} 分钟不在允许集合中"),
                });
            }
            write_setting(connection, "default_snooze_minutes", &minutes)
        }
    }
}

fn write_setting(
    connection: &mut Connection,
    key: &'static str,
    value: &impl serde::Serialize,
) -> Result<CommandResult, ApplicationError> {
    let value_json = serde_json::to_string(value)
        .map_err(|error| ApplicationError::storage("encode_setting", error))?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_command", error))?;
    transaction
        .execute(
            "INSERT INTO settings(key, value_json, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at_ms = excluded.updated_at_ms",
            params![key, value_json, now_ms()],
        )
        .map_err(|error| ApplicationError::storage("update_settings", error))?;
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_command", error))?;
    Ok(CommandResult::Applied)
}

fn save_document(
    connection: &mut Connection,
    request: SaveRequest,
    fail_after_write: bool,
) -> Result<EditingView, ApplicationError> {
    if request.persisted_note_id.is_none() && request.body.trim().is_empty() {
        return Err(ApplicationError::InvalidCommand {
            message: "空白草稿不能创建持久化便签".to_owned(),
        });
    }
    let revision = i64::try_from(request.revision)
        .map_err(|error| ApplicationError::storage("encode_content_revision", error))?;
    let title = derive_title(&request.body);
    let timestamp = now_ms();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_save_note", error))?;

    match request.persisted_note_id {
        Some(note_id) => {
            let changed = transaction
                .execute(
                    "UPDATE notes
                     SET body = ?1, derived_title = ?2, content_revision = ?3, updated_at_ms = ?4
                     WHERE id = ?5 AND lifecycle = 'active' AND content_revision < ?3",
                    params![
                        request.body,
                        title,
                        revision,
                        timestamp,
                        note_id.as_bytes().as_slice()
                    ],
                )
                .map_err(|error| ApplicationError::storage("update_note_body", error))?;
            if changed != 1 {
                return Err(ApplicationError::Storage {
                    operation: "update_note_body",
                    message: "便签不存在、不是活跃便签或修订已经过时".to_owned(),
                });
            }
        }
        None => {
            transaction
                .execute(
                    "INSERT INTO notes(
                        id, body, derived_title, content_revision, lifecycle,
                        created_at_ms, updated_at_ms, archived_at_ms, trashed_at_ms, due_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, NULL, NULL, NULL)",
                    params![
                        request.candidate_note_id.as_bytes().as_slice(),
                        request.body,
                        title,
                        revision,
                        timestamp
                    ],
                )
                .map_err(|error| ApplicationError::storage("create_note", error))?;
            if fail_after_write {
                return Err(injected_transaction_failure("save_note"));
            }
            transaction
                .execute(
                    "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)
                     ON CONFLICT(singleton) DO UPDATE SET note_id = excluded.note_id",
                    params![request.candidate_note_id.as_bytes().as_slice()],
                )
                .map_err(|error| ApplicationError::storage("set_created_note_current", error))?;
        }
    }

    if fail_after_write && request.persisted_note_id.is_some() {
        return Err(injected_transaction_failure("save_note"));
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_save_note", error))?;
    read_editing_view(connection)
}

fn switch_current_note(
    connection: &mut Connection,
    note_id: Uuid,
    fail_after_write: bool,
) -> Result<EditingView, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_switch_current", error))?;
    let target_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND lifecycle = 'active')",
            params![note_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("find_switch_target", error))?;
    if !target_exists {
        return Err(ApplicationError::InvalidCommand {
            message: "只能把活跃便签设为当前便签".to_owned(),
        });
    }
    transaction
        .execute(
            "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET note_id = excluded.note_id",
            params![note_id.as_bytes().as_slice()],
        )
        .map_err(|error| ApplicationError::storage("switch_current_note", error))?;
    if fail_after_write {
        return Err(injected_transaction_failure("switch_current_note"));
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_switch_current", error))?;
    read_editing_view(connection)
}

fn injected_transaction_failure(operation: &'static str) -> ApplicationError {
    ApplicationError::Storage {
        operation,
        message: "已注入提交前事务失败".to_owned(),
    }
}

fn verify_cross_table_invariants(transaction: &Transaction<'_>) -> Result<(), ApplicationError> {
    let active_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE lifecycle = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("check_active_notes", error))?;
    let current_count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM current_note", [], |row| row.get(0))
        .map_err(|error| ApplicationError::storage("check_current_note", error))?;
    let invalid_current: i64 = transaction
        .query_row(
            "SELECT COUNT(*)
             FROM current_note AS current
             JOIN notes AS note ON note.id = current.note_id
             WHERE note.lifecycle <> 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("check_current_lifecycle", error))?;
    let invalid_reminders: i64 = transaction
        .query_row(
            "SELECT COUNT(*)
             FROM reminders AS reminder
             JOIN notes AS note ON note.id = reminder.note_id
             WHERE note.lifecycle <> 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("check_reminder_lifecycle", error))?;

    let current_shape_is_valid =
        (active_count == 0 && current_count == 0) || (active_count > 0 && current_count == 1);
    if !current_shape_is_valid || invalid_current != 0 || invalid_reminders != 0 {
        return Err(ApplicationError::Storage {
            operation: "check_domain_invariants",
            message: "当前便签或提醒跨表不变量被破坏".to_owned(),
        });
    }
    Ok(())
}

fn read_snapshot(connection: &Connection) -> Result<ApplicationSnapshot, ApplicationError> {
    let application_id: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|error| ApplicationError::storage("read_application_id", error))?;
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| ApplicationError::storage("read_schema_version", error))?;
    let active_note_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE lifecycle = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("read_active_note_count", error))?;
    let active_note_count = active_note_count
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_active_note_count", error))?;
    let current_note_bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT note_id FROM current_note WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_current_note", error))?;
    let current_note_id = decode_optional_uuid(current_note_bytes, "decode_current_note")?;
    let default_snooze_minutes: u16 = read_json_setting(
        connection,
        "default_snooze_minutes",
        "read_default_snooze_minutes",
    )?;
    let shortcut_text: String =
        read_json_setting(connection, "global_shortcut", "read_global_shortcut")?;
    let global_shortcut = GlobalShortcut::parse(&shortcut_text)
        .map_err(|error| ApplicationError::storage("decode_global_shortcut", error))?;

    Ok(ApplicationSnapshot {
        schema: SchemaIdentity {
            application_id,
            version,
        },
        active_note_count,
        current_note_id,
        default_snooze_minutes,
        global_shortcut,
    })
}

fn read_json_setting<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    key: &'static str,
    operation: &'static str,
) -> Result<T, ApplicationError> {
    let value_json: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage(operation, error))?;
    serde_json::from_str(&value_json)
        .map_err(|error| ApplicationError::storage("decode_setting", error))
}

fn read_editing_view(connection: &Connection) -> Result<EditingView, ApplicationError> {
    let stored: Option<(Vec<u8>, String, String, i64)> = connection
        .query_row(
            "SELECT note.id, note.body, note.derived_title, note.content_revision
             FROM current_note AS current
             JOIN notes AS note ON note.id = current.note_id
             WHERE current.singleton = 1 AND note.lifecycle = 'active'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_editing_document", error))?;
    let document = match stored {
        Some((id, body, title, revision)) => StoredDocument {
            note_id: Some(decode_uuid(id, "decode_editing_note")?),
            body,
            title,
            revision: revision
                .try_into()
                .map_err(|error| ApplicationError::storage("decode_content_revision", error))?,
        },
        None => StoredDocument {
            note_id: None,
            body: String::new(),
            title: String::new(),
            revision: 0,
        },
    };

    Ok(EditingView {
        document,
        active_notes: read_active_notes(connection)?,
    })
}

fn read_active_notes(connection: &Connection) -> Result<Vec<NoteSummary>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT note.id, note.derived_title, current.note_id IS NOT NULL
             FROM notes AS note
             LEFT JOIN current_note AS current ON current.note_id = note.id
             WHERE note.lifecycle = 'active'
             ORDER BY note.updated_at_ms DESC, note.id",
        )
        .map_err(|error| ApplicationError::storage("prepare_active_notes", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .map_err(|error| ApplicationError::storage("read_active_notes", error))?;
    let mut notes = Vec::new();
    for row in rows {
        let (id, title, is_current) =
            row.map_err(|error| ApplicationError::storage("decode_active_note", error))?;
        notes.push(NoteSummary {
            id: decode_uuid(id, "decode_active_note_id")?,
            title,
            is_current,
        });
    }
    Ok(notes)
}

fn decode_optional_uuid(
    bytes: Option<Vec<u8>>,
    operation: &'static str,
) -> Result<Option<Uuid>, ApplicationError> {
    bytes.map(|bytes| decode_uuid(bytes, operation)).transpose()
}

fn decode_uuid(bytes: Vec<u8>, operation: &'static str) -> Result<Uuid, ApplicationError> {
    Uuid::from_slice(&bytes).map_err(|error| ApplicationError::storage(operation, error))
}

/// 标题只从第一条非空行派生，按字符截断以保护中文边界。
pub(crate) fn derive_title(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_default()
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
    use super::derive_title;

    #[test]
    fn title_uses_first_non_empty_line_without_splitting_unicode() {
        assert_eq!(derive_title("  \n  第一行标题  \n第二行"), "第一行标题");
        assert_eq!(derive_title(" \n\t"), "");
        let long = "便".repeat(100);
        assert_eq!(derive_title(&long).chars().count(), 80);
    }
}
