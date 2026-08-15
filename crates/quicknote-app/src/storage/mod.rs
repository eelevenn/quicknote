//! SQLite 单写者与应用命令实现。

mod migrations;

use crate::{
    ApplicationConfig, ApplicationError, ApplicationSnapshot, Command, CommandResult,
    SchemaIdentity,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
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

enum StorageRequest {
    Execute {
        command: Command,
        respond_to: Sender<Result<CommandResult, ApplicationError>>,
    },
    Snapshot {
        respond_to: Sender<Result<ApplicationSnapshot, ApplicationError>>,
    },
    Shutdown,
}

/// 独占 SQLite 连接并串行处理所有读写请求。
pub(crate) struct StorageWriter {
    sender: Sender<StorageRequest>,
    worker: Option<JoinHandle<()>>,
}

impl StorageWriter {
    /// 把已迁移连接移动到唯一后台线程。
    pub(crate) fn start(connection: Connection) -> Result<Self, ApplicationError> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("quicknote-sqlite-writer".to_owned())
            .spawn(move || run_writer(connection, receiver))
            .map_err(|error| ApplicationError::WriterUnavailable {
                message: error.to_string(),
            })?;
        Ok(Self {
            sender,
            worker: Some(worker),
        })
    }

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
}

impl Drop for StorageWriter {
    fn drop(&mut self) {
        // 正常退出先让独占线程完成队列中的请求，再回收连接。
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

fn run_writer(mut connection: Connection, receiver: Receiver<StorageRequest>) {
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

            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| ApplicationError::storage("begin_command", error))?;
            transaction
                .execute(
                    "INSERT INTO settings(key, value_json, updated_at_ms)
                     VALUES ('default_snooze_minutes', ?1, unixepoch('subsec') * 1000)
                     ON CONFLICT(key) DO UPDATE SET
                        value_json = excluded.value_json,
                        updated_at_ms = excluded.updated_at_ms",
                    params![minutes.to_string()],
                )
                .map_err(|error| ApplicationError::storage("update_settings", error))?;
            verify_cross_table_invariants(&transaction)?;
            transaction
                .commit()
                .map_err(|error| ApplicationError::storage("commit_command", error))?;
            Ok(CommandResult::Applied)
        }
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
    let current_note_id = current_note_bytes
        .map(|bytes| {
            Uuid::from_slice(&bytes)
                .map_err(|error| ApplicationError::storage("decode_current_note", error))
        })
        .transpose()?;
    let snooze_json: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'default_snooze_minutes'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("read_settings", error))?;
    let default_snooze_minutes: u16 = serde_json::from_str(&snooze_json)
        .map_err(|error| ApplicationError::storage("decode_settings", error))?;

    Ok(ApplicationSnapshot {
        schema: SchemaIdentity {
            application_id,
            version,
        },
        active_note_count,
        current_note_id,
        default_snooze_minutes,
    })
}
