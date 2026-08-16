//! SQLite 单写者与应用命令实现。

mod migrations;

use crate::platform::{GlobalShortcut, NotificationProjectionKey, PlatformCommand};
use crate::{
    ApplicationConfig, ApplicationError, ApplicationSnapshot, Command, CommandResult, ExportBundle,
    ExportReminder, ExportSettings, LibrarySnapshot, NoteAction, NoteDocument, NoteLifecycle,
    NoteSummary, NoteTiming, ReminderActivation, ReminderActivationAction,
    ReminderActivationOutcome, ReminderSnapshot, ReminderStatus, SchemaIdentity, SearchResult,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const TRASH_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const PERFORMANCE_GUARANTEE_BODY_BYTES: usize = 1024 * 1024;
const OUTBOX_BATCH_LIMIT: usize = 64;

/// 维护截止时间独立于存储请求流量，避免活跃应用无限推迟清理。
struct MaintenanceSchedule {
    next_run: Instant,
}

impl MaintenanceSchedule {
    fn new(started_at: Instant) -> Self {
        Self {
            next_run: started_at + MAINTENANCE_INTERVAL,
        }
    }

    fn wait_from(&self, now: Instant) -> Duration {
        self.next_run.saturating_duration_since(now)
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.next_run
    }

    fn mark_completed(&mut self, completed_at: Instant) {
        self.next_run = completed_at + MAINTENANCE_INTERVAL;
    }
}

/// `schedule` outbox 的完整、不可变通知载荷。
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct ReminderSchedulePayload {
    note_id: Uuid,
    title: String,
    body: String,
    snooze_minutes: u16,
    open_activation_key: String,
    snooze_activation_key: String,
    scheduled_at_ms: i64,
}

/// SQLite 左联接返回的一张便签时间行。
type NoteTimingRow = (
    Option<i64>,
    Option<Vec<u8>>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    bool,
    Option<String>,
);

/// 已提交、等待平台 adapter 应用的一条投影。
#[derive(Clone, Debug)]
pub(crate) struct ReminderProjection {
    pub(crate) outbox_id: i64,
    pub(crate) command: PlatformCommand,
}

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
    PersistStartupEnabled {
        enabled: bool,
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
    LibrarySnapshot {
        respond_to: Sender<Result<LibrarySnapshot, ApplicationError>>,
    },
    ReadNote {
        note_id: Uuid,
        respond_to: Sender<Result<NoteDocument, ApplicationError>>,
    },
    Search {
        query: String,
        respond_to: Sender<Result<Vec<SearchResult>, ApplicationError>>,
    },
    ApplyNoteAction {
        action: NoteAction,
        respond_to: Sender<Result<(), ApplicationError>>,
    },
    SetDueAt {
        note_id: Uuid,
        due_at_ms: Option<i64>,
        timestamp: i64,
        respond_to: Sender<Result<NoteTiming, ApplicationError>>,
    },
    SetReminder {
        note_id: Uuid,
        scheduled_at_ms: Option<i64>,
        timestamp: i64,
        respond_to: Sender<Result<NoteTiming, ApplicationError>>,
    },
    ReadNoteTiming {
        note_id: Uuid,
        respond_to: Sender<Result<NoteTiming, ApplicationError>>,
    },
    RespondToDueReminder {
        note_id: Uuid,
        timestamp: i64,
        respond_to: Sender<Result<bool, ApplicationError>>,
    },
    HandleReminderActivation {
        activation: ReminderActivation,
        timestamp: i64,
        respond_to: Sender<Result<ReminderActivationOutcome, ApplicationError>>,
    },
    AdvanceDueReminders {
        timestamp: i64,
        focused_note_id: Option<Uuid>,
        focused_since_ms: Option<i64>,
        cancel_missed_projection: bool,
        respond_to: Sender<Result<u64, ApplicationError>>,
    },
    ReconcileReminderProjections {
        scheduled: Vec<NotificationProjectionKey>,
        timestamp: i64,
        respond_to: Sender<Result<(), ApplicationError>>,
    },
    ReadyReminderProjections {
        timestamp: i64,
        respond_to: Sender<Result<Vec<ReminderProjection>, ApplicationError>>,
    },
    CompleteReminderProjection {
        outbox_id: i64,
        respond_to: Sender<Result<(), ApplicationError>>,
    },
    FailReminderProjection {
        outbox_id: i64,
        timestamp: i64,
        error: String,
        respond_to: Sender<Result<(), ApplicationError>>,
    },
    PendingReminderProjectionCount {
        respond_to: Sender<Result<u64, ApplicationError>>,
    },
    MaintainTrash {
        respond_to: Sender<Result<u64, ApplicationError>>,
    },
    ExportBundle {
        respond_to: Sender<Result<ExportBundle, ApplicationError>>,
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

    pub(crate) fn persist_startup_enabled(&self, enabled: bool) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::PersistStartupEnabled {
                enabled,
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

    pub(crate) fn library_snapshot(&self) -> Result<LibrarySnapshot, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::LibrarySnapshot { respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn read_note(&self, note_id: Uuid) -> Result<NoteDocument, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ReadNote {
                note_id,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn search(&self, query: &str) -> Result<Vec<SearchResult>, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::Search {
                query: query.to_owned(),
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn apply_note_action(&self, action: NoteAction) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ApplyNoteAction { action, respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn set_due_at(
        &self,
        note_id: Uuid,
        due_at_ms: Option<i64>,
        timestamp: i64,
    ) -> Result<NoteTiming, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::SetDueAt {
                note_id,
                due_at_ms,
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn set_reminder(
        &self,
        note_id: Uuid,
        scheduled_at_ms: Option<i64>,
        timestamp: i64,
    ) -> Result<NoteTiming, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::SetReminder {
                note_id,
                scheduled_at_ms,
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn read_note_timing(&self, note_id: Uuid) -> Result<NoteTiming, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ReadNoteTiming {
                note_id,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn respond_to_due_reminder(
        &self,
        note_id: Uuid,
        timestamp: i64,
    ) -> Result<bool, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::RespondToDueReminder {
                note_id,
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn handle_reminder_activation(
        &self,
        activation: ReminderActivation,
        timestamp: i64,
    ) -> Result<ReminderActivationOutcome, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::HandleReminderActivation {
                activation,
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn advance_due_reminders(
        &self,
        timestamp: i64,
        focused_note_id: Option<Uuid>,
        focused_since_ms: Option<i64>,
        cancel_missed_projection: bool,
    ) -> Result<u64, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::AdvanceDueReminders {
                timestamp,
                focused_note_id,
                focused_since_ms,
                cancel_missed_projection,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn reconcile_reminder_projections(
        &self,
        scheduled: Vec<NotificationProjectionKey>,
        timestamp: i64,
    ) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ReconcileReminderProjections {
                scheduled,
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn ready_reminder_projections(
        &self,
        timestamp: i64,
    ) -> Result<Vec<ReminderProjection>, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ReadyReminderProjections {
                timestamp,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn complete_reminder_projection(
        &self,
        outbox_id: i64,
    ) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::CompleteReminderProjection {
                outbox_id,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn fail_reminder_projection(
        &self,
        outbox_id: i64,
        timestamp: i64,
        error: String,
    ) -> Result<(), ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::FailReminderProjection {
                outbox_id,
                timestamp,
                error,
                respond_to,
            })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn pending_reminder_projection_count(&self) -> Result<u64, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::PendingReminderProjectionCount { respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn maintain_trash(&self) -> Result<u64, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::MaintainTrash { respond_to })
            .map_err(writer_disconnected)?;
        response.recv().map_err(writer_disconnected)?
    }

    pub(crate) fn export_bundle(&self) -> Result<ExportBundle, ApplicationError> {
        let (respond_to, response) = mpsc::channel();
        self.sender
            .send(StorageRequest::ExportBundle { respond_to })
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
    let mut maintenance = MaintenanceSchedule::new(Instant::now());
    loop {
        let request = match receiver.recv_timeout(maintenance.wait_from(Instant::now())) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => {
                maintain_trash_if_due(&mut connection, &mut maintenance);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
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
            StorageRequest::PersistStartupEnabled {
                enabled,
                respond_to,
            } => {
                let result =
                    write_setting(&mut connection, "startup_enabled", &enabled).map(|_| ());
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
            StorageRequest::LibrarySnapshot { respond_to } => {
                let _ = respond_to.send(read_library_snapshot(&connection));
            }
            StorageRequest::ReadNote {
                note_id,
                respond_to,
            } => {
                let _ = respond_to.send(read_note(&connection, note_id));
            }
            StorageRequest::Search { query, respond_to } => {
                let _ = respond_to.send(search_notes(&connection, &query));
            }
            StorageRequest::ApplyNoteAction { action, respond_to } => {
                let _ = respond_to.send(apply_note_action(&mut connection, action));
            }
            StorageRequest::SetDueAt {
                note_id,
                due_at_ms,
                timestamp,
                respond_to,
            } => {
                let _ = respond_to.send(set_due_at(&mut connection, note_id, due_at_ms, timestamp));
            }
            StorageRequest::SetReminder {
                note_id,
                scheduled_at_ms,
                timestamp,
                respond_to,
            } => {
                let _ = respond_to.send(set_reminder(
                    &mut connection,
                    note_id,
                    scheduled_at_ms,
                    timestamp,
                ));
            }
            StorageRequest::ReadNoteTiming {
                note_id,
                respond_to,
            } => {
                let _ = respond_to.send(read_note_timing(&connection, note_id));
            }
            StorageRequest::RespondToDueReminder {
                note_id,
                timestamp,
                respond_to,
            } => {
                let _ =
                    respond_to.send(respond_to_due_reminder(&mut connection, note_id, timestamp));
            }
            StorageRequest::HandleReminderActivation {
                activation,
                timestamp,
                respond_to,
            } => {
                let _ = respond_to.send(handle_reminder_activation(
                    &mut connection,
                    activation,
                    timestamp,
                ));
            }
            StorageRequest::AdvanceDueReminders {
                timestamp,
                focused_note_id,
                focused_since_ms,
                cancel_missed_projection,
                respond_to,
            } => {
                let _ = respond_to.send(advance_due_reminders(
                    &mut connection,
                    timestamp,
                    focused_note_id,
                    focused_since_ms,
                    cancel_missed_projection,
                ));
            }
            StorageRequest::ReconcileReminderProjections {
                scheduled,
                timestamp,
                respond_to,
            } => {
                let _ = respond_to.send(reconcile_reminder_projections(
                    &mut connection,
                    &scheduled,
                    timestamp,
                ));
            }
            StorageRequest::ReadyReminderProjections {
                timestamp,
                respond_to,
            } => {
                let _ =
                    respond_to.send(read_ready_reminder_projections(&mut connection, timestamp));
            }
            StorageRequest::CompleteReminderProjection {
                outbox_id,
                respond_to,
            } => {
                let _ = respond_to.send(complete_reminder_projection(&mut connection, outbox_id));
            }
            StorageRequest::FailReminderProjection {
                outbox_id,
                timestamp,
                error,
                respond_to,
            } => {
                let _ = respond_to.send(fail_reminder_projection(
                    &mut connection,
                    outbox_id,
                    timestamp,
                    &error,
                ));
            }
            StorageRequest::PendingReminderProjectionCount { respond_to } => {
                let _ = respond_to.send(pending_reminder_projection_count(&connection));
            }
            StorageRequest::MaintainTrash { respond_to } => {
                let _ = respond_to.send(purge_expired_trash(&mut connection, now_ms()));
            }
            StorageRequest::ExportBundle { respond_to } => {
                let _ = respond_to.send(read_export_bundle(&mut connection));
            }
            StorageRequest::Shutdown => break,
        }
        maintain_trash_if_due(&mut connection, &mut maintenance);
    }
    // 正常退出收敛 WAL，失败只影响维护状态，不破坏已提交事务。
    let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

fn maintain_trash_if_due(connection: &mut Connection, maintenance: &mut MaintenanceSchedule) {
    let now = Instant::now();
    if !maintenance.is_due(now) {
        return;
    }
    // 后台维护失败不终止单写者；下一次显式维护仍会返回错误。
    let _ = purge_expired_trash(connection, now_ms());
    maintenance.mark_completed(now);
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

fn set_due_at(
    connection: &mut Connection,
    note_id: Uuid,
    due_at_ms: Option<i64>,
    timestamp: i64,
) -> Result<NoteTiming, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_set_due_at", error))?;
    ensure_active_note(&transaction, note_id)?;
    transaction
        .execute(
            "UPDATE notes SET due_at_ms = ?1 WHERE id = ?2 AND lifecycle = 'active'",
            params![due_at_ms, note_id.as_bytes().as_slice()],
        )
        .map_err(|error| ApplicationError::storage("set_due_at", error))?;

    let has_reminder: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reminders WHERE note_id = ?1)",
            params![note_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("find_due_reminder", error))?;
    // 首次添加未来截止时间时提供同一时刻提醒，但之后两者保持独立。
    if !has_reminder && due_at_ms.is_some_and(|due| due > timestamp) {
        create_or_replace_reminder(
            &transaction,
            note_id,
            due_at_ms.expect("已验证未来截止时间"),
            timestamp,
        )?;
    }

    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_set_due_at", error))?;
    read_note_timing(connection, note_id)
}

fn set_reminder(
    connection: &mut Connection,
    note_id: Uuid,
    scheduled_at_ms: Option<i64>,
    timestamp: i64,
) -> Result<NoteTiming, ApplicationError> {
    if scheduled_at_ms.is_some_and(|scheduled| scheduled <= timestamp) {
        return Err(ApplicationError::InvalidCommand {
            message: "提醒时间必须位于未来".to_owned(),
        });
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_set_reminder", error))?;
    ensure_active_note(&transaction, note_id)?;
    match scheduled_at_ms {
        Some(scheduled_at_ms) => {
            create_or_replace_reminder(&transaction, note_id, scheduled_at_ms, timestamp)?;
        }
        None => {
            clear_note_reminder(&transaction, note_id, timestamp)?;
        }
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_set_reminder", error))?;
    read_note_timing(connection, note_id)
}

fn ensure_active_note(connection: &Connection, note_id: Uuid) -> Result<(), ApplicationError> {
    let active: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND lifecycle = 'active')",
            params![note_id.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("find_timing_note", error))?;
    if !active {
        return Err(ApplicationError::InvalidCommand {
            message: "只有活跃便签可以修改截止时间或提醒".to_owned(),
        });
    }
    Ok(())
}

fn create_or_replace_reminder(
    transaction: &Transaction<'_>,
    note_id: Uuid,
    scheduled_at_ms: i64,
    timestamp: i64,
) -> Result<(Uuid, u64), ApplicationError> {
    let existing: Option<(Vec<u8>, i64)> = transaction
        .query_row(
            "SELECT id, trigger_version FROM reminders WHERE note_id = ?1",
            params![note_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_existing_reminder", error))?;
    let (reminder_id, trigger_version) = match existing {
        Some((id, version)) => {
            let reminder_id = decode_uuid(id, "decode_existing_reminder")?;
            let old_version: u64 = version
                .try_into()
                .map_err(|error| ApplicationError::storage("decode_existing_trigger", error))?;
            queue_cancel_projection(transaction, reminder_id, old_version, timestamp)?;
            let trigger_version = old_version.saturating_add(1);
            let encoded_version = i64::try_from(trigger_version)
                .map_err(|error| ApplicationError::storage("encode_trigger_version", error))?;
            transaction
                .execute(
                    "UPDATE reminders
                     SET scheduled_at_ms = ?1, status = 'scheduled', trigger_version = ?2,
                         updated_at_ms = ?3
                     WHERE id = ?4",
                    params![
                        scheduled_at_ms,
                        encoded_version,
                        timestamp,
                        reminder_id.as_bytes().as_slice()
                    ],
                )
                .map_err(|error| ApplicationError::storage("replace_reminder", error))?;
            (reminder_id, trigger_version)
        }
        None => {
            let reminder_id = Uuid::now_v7();
            transaction
                .execute(
                    "INSERT INTO reminders(
                        id, note_id, scheduled_at_ms, status, trigger_version,
                        created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, 'scheduled', 1, ?4, ?4)",
                    params![
                        reminder_id.as_bytes().as_slice(),
                        note_id.as_bytes().as_slice(),
                        scheduled_at_ms,
                        timestamp
                    ],
                )
                .map_err(|error| ApplicationError::storage("create_reminder", error))?;
            (reminder_id, 1)
        }
    };
    queue_schedule_projection(
        transaction,
        reminder_id,
        note_id,
        trigger_version,
        scheduled_at_ms,
        timestamp,
    )?;
    Ok((reminder_id, trigger_version))
}

fn clear_note_reminder(
    transaction: &Transaction<'_>,
    note_id: Uuid,
    timestamp: i64,
) -> Result<bool, ApplicationError> {
    let existing: Option<(Vec<u8>, i64)> = transaction
        .query_row(
            "SELECT id, trigger_version FROM reminders WHERE note_id = ?1",
            params![note_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("find_cleared_reminder", error))?;
    let Some((reminder_id, trigger_version)) = existing else {
        return Ok(false);
    };
    let reminder_id = decode_uuid(reminder_id, "decode_cleared_reminder")?;
    let trigger_version = trigger_version
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_cleared_trigger", error))?;
    queue_cancel_projection(transaction, reminder_id, trigger_version, timestamp)?;
    transaction
        .execute(
            "DELETE FROM reminders WHERE id = ?1",
            params![reminder_id.as_bytes().as_slice()],
        )
        .map_err(|error| ApplicationError::storage("clear_reminder", error))?;
    Ok(true)
}

fn queue_schedule_projection(
    transaction: &Transaction<'_>,
    reminder_id: Uuid,
    note_id: Uuid,
    trigger_version: u64,
    scheduled_at_ms: i64,
    timestamp: i64,
) -> Result<(), ApplicationError> {
    let (title, body): (String, String) = transaction
        .query_row(
            "SELECT derived_title, body FROM notes WHERE id = ?1 AND lifecycle = 'active'",
            params![note_id.as_bytes().as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| ApplicationError::storage("read_notification_content", error))?;
    let snooze_minutes: u16 = read_json_setting(
        transaction,
        "default_snooze_minutes",
        "read_notification_snooze",
    )?;
    let payload = ReminderSchedulePayload {
        note_id,
        title: if title.is_empty() {
            "QuickNote 提醒".to_owned()
        } else {
            title
        },
        body: notification_body(&body),
        snooze_minutes,
        open_activation_key: Uuid::now_v7().to_string(),
        snooze_activation_key: Uuid::now_v7().to_string(),
        scheduled_at_ms,
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| ApplicationError::storage("encode_notification_payload", error))?;
    transaction
        .execute(
            "INSERT INTO reminder_outbox(
                reminder_id, trigger_version, projection_kind, payload_json,
                attempt_count, next_attempt_at_ms, last_error, delivered_at_ms
             ) VALUES (?1, ?2, 'schedule', ?3, 0, ?4, NULL, NULL)
             ON CONFLICT(reminder_id, trigger_version, projection_kind) DO UPDATE SET
                payload_json = excluded.payload_json,
                attempt_count = 0,
                next_attempt_at_ms = excluded.next_attempt_at_ms,
                last_error = NULL,
                delivered_at_ms = NULL",
            params![
                reminder_id.as_bytes().as_slice(),
                i64::try_from(trigger_version)
                    .map_err(|error| ApplicationError::storage("encode_schedule_trigger", error))?,
                payload_json,
                timestamp
            ],
        )
        .map_err(|error| ApplicationError::storage("queue_notification_schedule", error))?;
    Ok(())
}

fn queue_cancel_projection(
    transaction: &Transaction<'_>,
    reminder_id: Uuid,
    trigger_version: u64,
    timestamp: i64,
) -> Result<(), ApplicationError> {
    transaction
        .execute(
            "INSERT INTO reminder_outbox(
                reminder_id, trigger_version, projection_kind, payload_json,
                attempt_count, next_attempt_at_ms, last_error, delivered_at_ms
             ) VALUES (?1, ?2, 'cancel', '{}', 0, ?3, NULL, NULL)
             ON CONFLICT(reminder_id, trigger_version, projection_kind) DO UPDATE SET
                attempt_count = 0,
                next_attempt_at_ms = excluded.next_attempt_at_ms,
                last_error = NULL,
                delivered_at_ms = NULL",
            params![
                reminder_id.as_bytes().as_slice(),
                i64::try_from(trigger_version)
                    .map_err(|error| ApplicationError::storage("encode_cancel_trigger", error))?,
                timestamp
            ],
        )
        .map_err(|error| ApplicationError::storage("queue_notification_cancel", error))?;
    Ok(())
}

fn notification_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(240).collect()
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

fn apply_note_action(
    connection: &mut Connection,
    action: NoteAction,
) -> Result<(), ApplicationError> {
    let timestamp = now_ms();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_note_lifecycle", error))?;

    match action {
        NoteAction::Archive(note_id) => archive_note(&transaction, note_id, timestamp)?,
        NoteAction::Unarchive(note_id) => unarchive_note(&transaction, note_id, timestamp)?,
        NoteAction::MoveToTrash(note_id) => {
            transition_note(
                &transaction,
                note_id,
                "archived",
                "UPDATE notes SET lifecycle = 'trashed', trashed_at_ms = ?1
                 WHERE id = ?2 AND lifecycle = 'archived'",
                timestamp,
                "move_note_to_trash",
            )?;
        }
        NoteAction::RestoreFromTrash(note_id) => {
            transition_note(
                &transaction,
                note_id,
                "trashed",
                "UPDATE notes SET lifecycle = 'archived', trashed_at_ms = NULL
                 WHERE id = ?2 AND lifecycle = 'trashed'",
                timestamp,
                "restore_note_from_trash",
            )?;
        }
        NoteAction::PermanentlyDelete(note_id) => {
            let changed = transaction
                .execute(
                    "DELETE FROM notes WHERE id = ?1 AND lifecycle = 'trashed'",
                    params![note_id.as_bytes().as_slice()],
                )
                .map_err(|error| ApplicationError::storage("permanently_delete_note", error))?;
            if changed != 1 {
                return Err(invalid_lifecycle("只有回收站便签可以永久清除"));
            }
        }
    }

    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_note_lifecycle", error))
}

fn archive_note(
    transaction: &Transaction<'_>,
    note_id: Uuid,
    timestamp: i64,
) -> Result<(), ApplicationError> {
    let note_bytes = note_id.as_bytes().as_slice();
    let is_current: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM current_note WHERE note_id = ?1)",
            params![note_bytes],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("find_archived_current", error))?;

    // 先保留取消投影事实，再清除提醒，满足离开活跃状态的触发器约束。
    transaction
        .execute(
            "INSERT OR IGNORE INTO reminder_outbox(
                reminder_id, trigger_version, projection_kind, payload_json,
                attempt_count, next_attempt_at_ms, last_error, delivered_at_ms
             )
             SELECT id, trigger_version, 'cancel', '{}', 0, ?1, NULL, NULL
             FROM reminders WHERE note_id = ?2",
            params![timestamp, note_bytes],
        )
        .map_err(|error| ApplicationError::storage("queue_archived_reminder_cancel", error))?;
    transaction
        .execute(
            "DELETE FROM reminders WHERE note_id = ?1",
            params![note_bytes],
        )
        .map_err(|error| ApplicationError::storage("clear_archived_reminder", error))?;
    if is_current {
        transaction
            .execute(
                "DELETE FROM current_note WHERE note_id = ?1",
                params![note_bytes],
            )
            .map_err(|error| ApplicationError::storage("clear_archived_current", error))?;
    }
    let changed = transaction
        .execute(
            "UPDATE notes
             SET lifecycle = 'archived', archived_at_ms = ?1, trashed_at_ms = NULL
             WHERE id = ?2 AND lifecycle = 'active'",
            params![timestamp, note_bytes],
        )
        .map_err(|error| ApplicationError::storage("archive_note", error))?;
    if changed != 1 {
        return Err(invalid_lifecycle("只有活跃便签可以归档"));
    }

    if is_current {
        let successor: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT id FROM notes
                 WHERE lifecycle = 'active'
                 ORDER BY updated_at_ms DESC, id
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| ApplicationError::storage("select_archive_successor", error))?;
        if let Some(successor) = successor {
            transaction
                .execute(
                    "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)",
                    params![successor],
                )
                .map_err(|error| ApplicationError::storage("set_archive_successor", error))?;
        }
    }
    Ok(())
}

fn unarchive_note(
    transaction: &Transaction<'_>,
    note_id: Uuid,
    _timestamp: i64,
) -> Result<(), ApplicationError> {
    let note_bytes = note_id.as_bytes().as_slice();
    let changed = transaction
        .execute(
            "UPDATE notes
             SET lifecycle = 'active', archived_at_ms = NULL, trashed_at_ms = NULL
             WHERE id = ?1 AND lifecycle = 'archived'",
            params![note_bytes],
        )
        .map_err(|error| ApplicationError::storage("unarchive_note", error))?;
    if changed != 1 {
        return Err(invalid_lifecycle("只有归档便签可以取消归档"));
    }
    let has_current: bool = transaction
        .query_row("SELECT EXISTS(SELECT 1 FROM current_note)", [], |row| {
            row.get(0)
        })
        .map_err(|error| ApplicationError::storage("find_current_after_unarchive", error))?;
    if !has_current {
        transaction
            .execute(
                "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)",
                params![note_bytes],
            )
            .map_err(|error| ApplicationError::storage("set_unarchived_current", error))?;
    }
    Ok(())
}

fn transition_note(
    transaction: &Transaction<'_>,
    note_id: Uuid,
    _expected_lifecycle: &'static str,
    sql: &'static str,
    timestamp: i64,
    operation: &'static str,
) -> Result<(), ApplicationError> {
    let changed = transaction
        .execute(sql, params![timestamp, note_id.as_bytes().as_slice()])
        .map_err(|error| ApplicationError::storage(operation, error))?;
    if changed != 1 {
        return Err(invalid_lifecycle(match operation {
            "move_note_to_trash" => "活跃便签不能直接删除；请先归档",
            "restore_note_from_trash" => "只有回收站便签可以恢复到归档",
            _ => "便签生命周期转换无效",
        }));
    }
    Ok(())
}

fn invalid_lifecycle(message: &'static str) -> ApplicationError {
    ApplicationError::InvalidCommand {
        message: message.to_owned(),
    }
}

fn read_note_timing(
    connection: &Connection,
    note_id: Uuid,
) -> Result<NoteTiming, ApplicationError> {
    let row: Option<NoteTimingRow> = connection
        .query_row(
            "SELECT note.due_at_ms,
                    reminder.id,
                    reminder.scheduled_at_ms,
                    reminder.status,
                    reminder.trigger_version,
                    EXISTS(
                        SELECT 1 FROM reminder_outbox AS pending
                        WHERE pending.reminder_id = reminder.id
                          AND pending.delivered_at_ms IS NULL
                    ),
                    (
                        SELECT failed.last_error FROM reminder_outbox AS failed
                        WHERE failed.reminder_id = reminder.id
                          AND failed.delivered_at_ms IS NULL
                          AND failed.last_error IS NOT NULL
                        ORDER BY failed.id DESC LIMIT 1
                    )
             FROM notes AS note
             LEFT JOIN reminders AS reminder ON reminder.note_id = note.id
             WHERE note.id = ?1 AND note.lifecycle = 'active'",
            params![note_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_note_timing", error))?;
    let Some((
        due_at_ms,
        reminder_id,
        scheduled_at_ms,
        status,
        trigger_version,
        platform_sync_pending,
        platform_sync_error,
    )) = row
    else {
        return Err(ApplicationError::InvalidCommand {
            message: "只有活跃便签拥有可编辑的截止时间与提醒".to_owned(),
        });
    };
    let reminder = match (reminder_id, scheduled_at_ms, status, trigger_version) {
        (Some(id), Some(scheduled_at_ms), Some(status), Some(trigger_version)) => {
            let status = decode_reminder_status(&status)?;
            Some(ReminderSnapshot {
                id: decode_uuid(id, "decode_note_reminder")?,
                scheduled_at_ms,
                status,
                trigger_version: trigger_version.try_into().map_err(|error| {
                    ApplicationError::storage("decode_note_trigger_version", error)
                })?,
                platform_sync_pending,
                platform_sync_error: platform_sync_error.clone(),
            })
        }
        (None, None, None, None) => None,
        _ => {
            return Err(ApplicationError::Storage {
                operation: "decode_note_timing",
                message: "提醒联接结果不完整".to_owned(),
            });
        }
    };
    Ok(NoteTiming {
        note_id,
        due_at_ms,
        reminder,
        platform_sync_pending,
        platform_sync_error,
    })
}

fn respond_to_due_reminder(
    connection: &mut Connection,
    note_id: Uuid,
    timestamp: i64,
) -> Result<bool, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_respond_reminder", error))?;
    ensure_active_note(&transaction, note_id)?;
    let due: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM reminders
                WHERE note_id = ?1 AND scheduled_at_ms <= ?2
             )",
            params![note_id.as_bytes().as_slice(), timestamp],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("find_due_reminder", error))?;
    if due {
        clear_note_reminder(&transaction, note_id, timestamp)?;
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_respond_reminder", error))?;
    Ok(due)
}

fn handle_reminder_activation(
    connection: &mut Connection,
    activation: ReminderActivation,
    timestamp: i64,
) -> Result<ReminderActivationOutcome, ApplicationError> {
    if Uuid::parse_str(&activation.activation_key).is_err() {
        return Ok(ReminderActivationOutcome::Ignored);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_reminder_activation", error))?;
    let action_name = match activation.action {
        ReminderActivationAction::Open => "open",
        ReminderActivationAction::Snooze => "snooze",
    };
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO activation_receipts(
                id, activation_key, reminder_id, trigger_version, action, received_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::now_v7().as_bytes().as_slice(),
                activation.activation_key,
                activation.reminder_id.as_bytes().as_slice(),
                i64::try_from(activation.trigger_version).map_err(|error| {
                    ApplicationError::storage("encode_activation_trigger", error)
                })?,
                action_name,
                timestamp
            ],
        )
        .map_err(|error| ApplicationError::storage("record_reminder_activation", error))?;
    if inserted == 0 {
        transaction
            .commit()
            .map_err(|error| ApplicationError::storage("commit_duplicate_activation", error))?;
        return Ok(ReminderActivationOutcome::Ignored);
    }

    let note_id: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT reminder.note_id
             FROM reminders AS reminder
             JOIN notes AS note ON note.id = reminder.note_id
             WHERE reminder.id = ?1 AND reminder.trigger_version = ?2
               AND note.lifecycle = 'active'",
            params![
                activation.reminder_id.as_bytes().as_slice(),
                i64::try_from(activation.trigger_version).map_err(|error| {
                    ApplicationError::storage("encode_activation_lookup", error)
                })?
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("find_activation_reminder", error))?;
    let Some(note_id) = note_id else {
        transaction
            .commit()
            .map_err(|error| ApplicationError::storage("commit_stale_activation", error))?;
        return Ok(ReminderActivationOutcome::Ignored);
    };
    let note_id = decode_uuid(note_id, "decode_activation_note")?;

    let outcome = match activation.action {
        ReminderActivationAction::Open => {
            queue_cancel_projection(
                &transaction,
                activation.reminder_id,
                activation.trigger_version,
                timestamp,
            )?;
            transaction
                .execute(
                    "DELETE FROM reminders WHERE id = ?1 AND trigger_version = ?2",
                    params![
                        activation.reminder_id.as_bytes().as_slice(),
                        i64::try_from(activation.trigger_version).map_err(|error| {
                            ApplicationError::storage("encode_open_trigger", error)
                        })?
                    ],
                )
                .map_err(|error| ApplicationError::storage("respond_open_reminder", error))?;
            transaction
                .execute(
                    "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)
                     ON CONFLICT(singleton) DO UPDATE SET note_id = excluded.note_id",
                    params![note_id.as_bytes().as_slice()],
                )
                .map_err(|error| ApplicationError::storage("route_open_reminder", error))?;
            ReminderActivationOutcome::Opened { note_id }
        }
        ReminderActivationAction::Snooze => {
            let Some(minutes) = activation.snooze_minutes else {
                transaction
                    .commit()
                    .map_err(|error| ApplicationError::storage("commit_invalid_snooze", error))?;
                return Ok(ReminderActivationOutcome::Ignored);
            };
            if ![5, 10, 15, 30, 60].contains(&minutes) {
                transaction
                    .commit()
                    .map_err(|error| ApplicationError::storage("commit_invalid_snooze", error))?;
                return Ok(ReminderActivationOutcome::Ignored);
            }
            let scheduled_at_ms = timestamp.saturating_add(i64::from(minutes) * 60 * 1_000);
            queue_cancel_projection(
                &transaction,
                activation.reminder_id,
                activation.trigger_version,
                timestamp,
            )?;
            let next_version = activation.trigger_version.saturating_add(1);
            transaction
                .execute(
                    "UPDATE reminders
                     SET scheduled_at_ms = ?1, status = 'scheduled', trigger_version = ?2,
                         updated_at_ms = ?3
                     WHERE id = ?4 AND trigger_version = ?5",
                    params![
                        scheduled_at_ms,
                        i64::try_from(next_version).map_err(|error| {
                            ApplicationError::storage("encode_snoozed_trigger", error)
                        })?,
                        timestamp,
                        activation.reminder_id.as_bytes().as_slice(),
                        i64::try_from(activation.trigger_version).map_err(|error| {
                            ApplicationError::storage("encode_previous_trigger", error)
                        })?
                    ],
                )
                .map_err(|error| ApplicationError::storage("snooze_reminder", error))?;
            queue_schedule_projection(
                &transaction,
                activation.reminder_id,
                note_id,
                next_version,
                scheduled_at_ms,
                timestamp,
            )?;
            ReminderActivationOutcome::Snoozed {
                note_id,
                scheduled_at_ms,
            }
        }
    };
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_reminder_activation", error))?;
    Ok(outcome)
}

fn advance_due_reminders(
    connection: &mut Connection,
    timestamp: i64,
    focused_note_id: Option<Uuid>,
    focused_since_ms: Option<i64>,
    cancel_missed_projection: bool,
) -> Result<u64, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_advance_reminders", error))?;
    let due = {
        let mut statement = transaction
            .prepare(
                "SELECT id, note_id, scheduled_at_ms, trigger_version
                 FROM reminders
                 WHERE status = 'scheduled' AND scheduled_at_ms <= ?1
                 ORDER BY scheduled_at_ms, id",
            )
            .map_err(|error| ApplicationError::storage("prepare_due_reminders", error))?;
        let rows = statement
            .query_map(params![timestamp], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|error| ApplicationError::storage("read_due_reminders", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApplicationError::storage("decode_due_reminder", error))?
    };

    for (reminder_id, note_id, scheduled_at_ms, trigger_version) in &due {
        let reminder_id = decode_uuid(reminder_id.clone(), "decode_due_reminder_id")?;
        let note_id = decode_uuid(note_id.clone(), "decode_due_note_id")?;
        let trigger_version: u64 = (*trigger_version)
            .try_into()
            .map_err(|error| ApplicationError::storage("decode_due_trigger", error))?;
        let was_focused_at_due = focused_note_id == Some(note_id)
            && focused_since_ms
                .is_some_and(|since| since <= *scheduled_at_ms && *scheduled_at_ms <= timestamp);
        if was_focused_at_due {
            queue_cancel_projection(&transaction, reminder_id, trigger_version, timestamp)?;
            transaction
                .execute(
                    "DELETE FROM reminders WHERE id = ?1 AND trigger_version = ?2",
                    params![
                        reminder_id.as_bytes().as_slice(),
                        i64::try_from(trigger_version).map_err(|error| {
                            ApplicationError::storage("encode_focused_trigger", error)
                        })?
                    ],
                )
                .map_err(|error| ApplicationError::storage("respond_focused_reminder", error))?;
        } else {
            transaction
                .execute(
                    "UPDATE reminders SET status = 'missed', updated_at_ms = ?1
                     WHERE id = ?2 AND trigger_version = ?3",
                    params![
                        timestamp,
                        reminder_id.as_bytes().as_slice(),
                        i64::try_from(trigger_version).map_err(|error| {
                            ApplicationError::storage("encode_missed_trigger", error)
                        })?
                    ],
                )
                .map_err(|error| ApplicationError::storage("mark_reminder_missed", error))?;
            if cancel_missed_projection {
                queue_cancel_projection(&transaction, reminder_id, trigger_version, timestamp)?;
            }
        }
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_advance_reminders", error))?;
    due.len()
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_due_count", error))
}

fn reconcile_reminder_projections(
    connection: &mut Connection,
    scheduled: &[NotificationProjectionKey],
    timestamp: i64,
) -> Result<(), ApplicationError> {
    let platform_keys = scheduled.iter().copied().collect::<BTreeSet<_>>();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_reconcile_reminders", error))?;
    let domain_keys = {
        let mut statement = transaction
            .prepare(
                "SELECT id, note_id, trigger_version, scheduled_at_ms
                 FROM reminders
                 WHERE status = 'scheduled' AND scheduled_at_ms > ?1",
            )
            .map_err(|error| ApplicationError::storage("prepare_reconcile_reminders", error))?;
        let rows = statement
            .query_map(params![timestamp], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|error| ApplicationError::storage("read_reconcile_reminders", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApplicationError::storage("decode_reconcile_reminder", error))?
    };
    let mut current_keys = BTreeSet::new();
    for (reminder_id, note_id, trigger_version, scheduled_at_ms) in domain_keys {
        let reminder_id = decode_uuid(reminder_id, "decode_reconcile_reminder_id")?;
        let note_id = decode_uuid(note_id, "decode_reconcile_note_id")?;
        let trigger_version = trigger_version
            .try_into()
            .map_err(|error| ApplicationError::storage("decode_reconcile_trigger", error))?;
        let key = NotificationProjectionKey {
            reminder_id,
            trigger_version,
        };
        current_keys.insert(key);
        if platform_keys.contains(&key) {
            transaction
                .execute(
                    "UPDATE reminder_outbox SET delivered_at_ms = COALESCE(delivered_at_ms, ?1),
                        last_error = NULL
                     WHERE reminder_id = ?2 AND trigger_version = ?3
                       AND projection_kind = 'schedule'",
                    params![
                        timestamp,
                        reminder_id.as_bytes().as_slice(),
                        i64::try_from(trigger_version).map_err(|error| {
                            ApplicationError::storage("encode_reconciled_trigger", error)
                        })?
                    ],
                )
                .map_err(|error| ApplicationError::storage("accept_existing_schedule", error))?;
        } else {
            queue_schedule_projection(
                &transaction,
                reminder_id,
                note_id,
                trigger_version,
                scheduled_at_ms,
                timestamp,
            )?;
        }
    }
    for stale in platform_keys.difference(&current_keys) {
        queue_cancel_projection(
            &transaction,
            stale.reminder_id,
            stale.trigger_version,
            timestamp,
        )?;
    }
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_reconcile_reminders", error))
}

fn read_ready_reminder_projections(
    connection: &mut Connection,
    timestamp: i64,
) -> Result<Vec<ReminderProjection>, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_read_reminder_outbox", error))?;
    // 过时或已经错过的 schedule 绝不补发，直接收敛为已处理。
    transaction
        .execute(
            "UPDATE reminder_outbox AS outbox
             SET delivered_at_ms = ?1, last_error = NULL
             WHERE outbox.delivered_at_ms IS NULL
               AND outbox.projection_kind = 'schedule'
               AND NOT EXISTS(
                    SELECT 1 FROM reminders AS reminder
                    WHERE reminder.id = outbox.reminder_id
                      AND reminder.trigger_version = outbox.trigger_version
                      AND reminder.status = 'scheduled'
                      AND reminder.scheduled_at_ms > ?1
               )",
            params![timestamp],
        )
        .map_err(|error| ApplicationError::storage("discard_stale_schedule", error))?;
    let rows = {
        let mut statement = transaction
            .prepare(
                "SELECT id, reminder_id, trigger_version, projection_kind, payload_json
                 FROM reminder_outbox
                 WHERE delivered_at_ms IS NULL AND next_attempt_at_ms <= ?1
                 ORDER BY id LIMIT ?2",
            )
            .map_err(|error| ApplicationError::storage("prepare_reminder_outbox", error))?;
        let mapped = statement
            .query_map(params![timestamp, OUTBOX_BATCH_LIMIT as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|error| ApplicationError::storage("read_reminder_outbox", error))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApplicationError::storage("decode_reminder_outbox", error))?
    };
    let mut projections = Vec::with_capacity(rows.len());
    for (outbox_id, reminder_id, trigger_version, kind, payload_json) in rows {
        let reminder_id = decode_uuid(reminder_id, "decode_outbox_reminder")?;
        let trigger_version: u64 = trigger_version
            .try_into()
            .map_err(|error| ApplicationError::storage("decode_outbox_trigger", error))?;
        let command = match kind.as_str() {
            "schedule" => {
                let payload: ReminderSchedulePayload = serde_json::from_str(&payload_json)
                    .map_err(|error| ApplicationError::storage("decode_schedule_payload", error))?;
                PlatformCommand::UpsertNotification {
                    reminder_id,
                    trigger_version,
                    note_id: payload.note_id,
                    scheduled_at_ms: payload.scheduled_at_ms,
                    title: payload.title,
                    body: payload.body,
                    snooze_minutes: payload.snooze_minutes,
                    open_activation_key: payload.open_activation_key,
                    snooze_activation_key: payload.snooze_activation_key,
                }
            }
            "cancel" => PlatformCommand::CancelNotification {
                reminder_id,
                trigger_version,
            },
            other => {
                return Err(ApplicationError::Storage {
                    operation: "decode_outbox_projection",
                    message: format!("未知提醒投影 {other}"),
                });
            }
        };
        projections.push(ReminderProjection { outbox_id, command });
    }
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_read_reminder_outbox", error))?;
    Ok(projections)
}

fn complete_reminder_projection(
    connection: &mut Connection,
    outbox_id: i64,
) -> Result<(), ApplicationError> {
    connection
        .execute(
            "UPDATE reminder_outbox SET delivered_at_ms = ?1, last_error = NULL
             WHERE id = ?2 AND delivered_at_ms IS NULL",
            params![now_ms(), outbox_id],
        )
        .map_err(|error| ApplicationError::storage("complete_reminder_projection", error))?;
    Ok(())
}

fn fail_reminder_projection(
    connection: &mut Connection,
    outbox_id: i64,
    timestamp: i64,
    error: &str,
) -> Result<(), ApplicationError> {
    let attempt_count: i64 = connection
        .query_row(
            "SELECT attempt_count FROM reminder_outbox
             WHERE id = ?1 AND delivered_at_ms IS NULL",
            params![outbox_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_projection_attempt", error))?
        .unwrap_or(0);
    let next_attempt = attempt_count.saturating_add(1);
    let delay_ms = reminder_retry_delay_ms(next_attempt);
    connection
        .execute(
            "UPDATE reminder_outbox
             SET attempt_count = ?1, next_attempt_at_ms = ?2, last_error = ?3
             WHERE id = ?4 AND delivered_at_ms IS NULL",
            params![
                next_attempt,
                timestamp.saturating_add(delay_ms),
                error,
                outbox_id
            ],
        )
        .map_err(|error| ApplicationError::storage("fail_reminder_projection", error))?;
    Ok(())
}

fn reminder_retry_delay_ms(attempt_count: i64) -> i64 {
    match attempt_count {
        ..=1 => 1_000,
        2 => 5_000,
        3 => 30_000,
        4 => 5 * 60_000,
        5 => 30 * 60_000,
        _ => 6 * 60 * 60_000,
    }
}

fn pending_reminder_projection_count(connection: &Connection) -> Result<u64, ApplicationError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE delivered_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("count_pending_reminder_projection", error))?;
    count
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_pending_projection_count", error))
}

fn decode_reminder_status(value: &str) -> Result<ReminderStatus, ApplicationError> {
    match value {
        "scheduled" => Ok(ReminderStatus::Scheduled),
        "missed" => Ok(ReminderStatus::Missed),
        other => Err(ApplicationError::Storage {
            operation: "decode_reminder_status",
            message: format!("未知提醒状态 {other}"),
        }),
    }
}

fn decode_optional_reminder(
    reminder_id: Option<Vec<u8>>,
    scheduled_at_ms: Option<i64>,
    status: Option<String>,
    trigger_version: Option<i64>,
    platform_sync_pending: bool,
    platform_sync_error: Option<String>,
) -> Result<Option<ReminderSnapshot>, ApplicationError> {
    match (reminder_id, scheduled_at_ms, status, trigger_version) {
        (Some(id), Some(scheduled_at_ms), Some(status), Some(trigger_version)) => {
            Ok(Some(ReminderSnapshot {
                id: decode_uuid(id, "decode_summary_reminder")?,
                scheduled_at_ms,
                status: decode_reminder_status(&status)?,
                trigger_version: trigger_version
                    .try_into()
                    .map_err(|error| ApplicationError::storage("decode_summary_trigger", error))?,
                platform_sync_pending,
                platform_sync_error,
            }))
        }
        (None, None, None, None) => Ok(None),
        _ => Err(ApplicationError::Storage {
            operation: "decode_summary_reminder",
            message: "提醒摘要联接结果不完整".to_owned(),
        }),
    }
}

fn purge_expired_trash(
    connection: &mut Connection,
    timestamp: i64,
) -> Result<u64, ApplicationError> {
    let cutoff = timestamp.saturating_sub(TRASH_RETENTION_MS);
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| ApplicationError::storage("begin_trash_maintenance", error))?;
    let deleted = transaction
        .execute(
            "DELETE FROM notes
             WHERE lifecycle = 'trashed' AND trashed_at_ms <= ?1",
            params![cutoff],
        )
        .map_err(|error| ApplicationError::storage("purge_expired_trash", error))?;
    let timestamp_json = serde_json::to_string(&timestamp)
        .map_err(|error| ApplicationError::storage("encode_maintenance_cursor", error))?;
    transaction
        .execute(
            "INSERT INTO maintenance_state(key, value_json, updated_at_ms)
             VALUES ('last_trash_cleanup_ms', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at_ms = excluded.updated_at_ms",
            params![timestamp_json, timestamp],
        )
        .map_err(|error| ApplicationError::storage("write_maintenance_cursor", error))?;
    verify_cross_table_invariants(&transaction)?;
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_trash_maintenance", error))?;
    deleted
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_purged_note_count", error))
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
    let archived_note_count = read_lifecycle_count(connection, "archived")?;
    let trashed_note_count = read_lifecycle_count(connection, "trashed")?;
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
    let startup_enabled: bool =
        read_json_setting(connection, "startup_enabled", "read_startup_enabled")?;

    Ok(ApplicationSnapshot {
        schema: SchemaIdentity {
            application_id,
            version,
        },
        active_note_count,
        archived_note_count,
        trashed_note_count,
        current_note_id,
        default_snooze_minutes,
        global_shortcut,
        startup_enabled,
    })
}

fn read_lifecycle_count(
    connection: &Connection,
    lifecycle: &'static str,
) -> Result<u64, ApplicationError> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM notes WHERE lifecycle = ?1",
            params![lifecycle],
            |row| row.get(0),
        )
        .map_err(|error| ApplicationError::storage("read_lifecycle_count", error))?;
    count
        .try_into()
        .map_err(|error| ApplicationError::storage("decode_lifecycle_count", error))
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
            "SELECT note.id, note.derived_title, current.note_id IS NOT NULL, note.updated_at_ms,
                    note.due_at_ms, reminder.id, reminder.scheduled_at_ms,
                    reminder.status, reminder.trigger_version,
                    EXISTS(
                        SELECT 1 FROM reminder_outbox AS pending
                        WHERE pending.reminder_id = reminder.id
                          AND pending.delivered_at_ms IS NULL
                    ),
                    (
                        SELECT failed.last_error FROM reminder_outbox AS failed
                        WHERE failed.reminder_id = reminder.id
                          AND failed.delivered_at_ms IS NULL
                          AND failed.last_error IS NOT NULL
                        ORDER BY failed.id DESC LIMIT 1
                    )
             FROM notes AS note
             LEFT JOIN current_note AS current ON current.note_id = note.id
             LEFT JOIN reminders AS reminder ON reminder.note_id = note.id
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
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, bool>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(|error| ApplicationError::storage("read_active_notes", error))?;
    let mut notes = Vec::new();
    for row in rows {
        let (
            id,
            title,
            is_current,
            updated_at_ms,
            due_at_ms,
            reminder_id,
            scheduled_at_ms,
            reminder_status,
            trigger_version,
            platform_sync_pending,
            platform_sync_error,
        ) = row.map_err(|error| ApplicationError::storage("decode_active_note", error))?;
        notes.push(NoteSummary {
            id: decode_uuid(id, "decode_active_note_id")?,
            title,
            is_current,
            lifecycle: NoteLifecycle::Active,
            updated_at_ms,
            due_at_ms,
            reminder: decode_optional_reminder(
                reminder_id,
                scheduled_at_ms,
                reminder_status,
                trigger_version,
                platform_sync_pending,
                platform_sync_error,
            )?,
        });
    }
    Ok(notes)
}

fn read_library_snapshot(connection: &Connection) -> Result<LibrarySnapshot, ApplicationError> {
    let mut active = Vec::new();
    let mut archived = Vec::new();
    let mut trashed = Vec::new();
    let mut statement = connection
        .prepare(
            "SELECT note.id, note.derived_title, note.lifecycle,
                    current.note_id IS NOT NULL, note.updated_at_ms, note.due_at_ms,
                    reminder.id, reminder.scheduled_at_ms, reminder.status,
                    reminder.trigger_version,
                    EXISTS(
                        SELECT 1 FROM reminder_outbox AS pending
                        WHERE pending.reminder_id = reminder.id
                          AND pending.delivered_at_ms IS NULL
                    ),
                    (
                        SELECT failed.last_error FROM reminder_outbox AS failed
                        WHERE failed.reminder_id = reminder.id
                          AND failed.delivered_at_ms IS NULL
                          AND failed.last_error IS NOT NULL
                        ORDER BY failed.id DESC LIMIT 1
                    )
             FROM notes AS note
             LEFT JOIN current_note AS current ON current.note_id = note.id
             LEFT JOIN reminders AS reminder ON reminder.note_id = note.id
             ORDER BY
                CASE note.lifecycle WHEN 'active' THEN 0 WHEN 'archived' THEN 1 ELSE 2 END,
                CASE note.lifecycle
                    WHEN 'active' THEN note.updated_at_ms
                    WHEN 'archived' THEN note.archived_at_ms
                    ELSE note.trashed_at_ms
                END DESC,
                note.id",
        )
        .map_err(|error| ApplicationError::storage("prepare_library_snapshot", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<Vec<u8>>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, bool>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(|error| ApplicationError::storage("read_library_snapshot", error))?;
    for row in rows {
        let (
            id,
            title,
            lifecycle,
            is_current,
            updated_at_ms,
            due_at_ms,
            reminder_id,
            scheduled_at_ms,
            reminder_status,
            trigger_version,
            platform_sync_pending,
            platform_sync_error,
        ) = row.map_err(|error| ApplicationError::storage("decode_library_note", error))?;
        let lifecycle = decode_lifecycle(&lifecycle)?;
        let summary = NoteSummary {
            id: decode_uuid(id, "decode_library_note_id")?,
            title,
            is_current,
            lifecycle,
            updated_at_ms,
            due_at_ms,
            reminder: decode_optional_reminder(
                reminder_id,
                scheduled_at_ms,
                reminder_status,
                trigger_version,
                platform_sync_pending,
                platform_sync_error,
            )?,
        };
        match lifecycle {
            NoteLifecycle::Active => active.push(summary),
            NoteLifecycle::Archived => archived.push(summary),
            NoteLifecycle::Trashed => trashed.push(summary),
        }
    }
    Ok(LibrarySnapshot {
        active,
        archived,
        trashed,
    })
}

fn read_note(connection: &Connection, note_id: Uuid) -> Result<NoteDocument, ApplicationError> {
    connection
        .query_row(
            "SELECT id, body, derived_title, lifecycle, content_revision,
                    created_at_ms, updated_at_ms, archived_at_ms, trashed_at_ms, due_at_ms
             FROM notes WHERE id = ?1",
            params![note_id.as_bytes().as_slice()],
            decode_note_row,
        )
        .optional()
        .map_err(|error| ApplicationError::storage("read_note", error))?
        .ok_or_else(|| ApplicationError::InvalidCommand {
            message: "便签不存在或已经永久清除".to_owned(),
        })
}

fn search_notes(
    connection: &Connection,
    query: &str,
) -> Result<Vec<SearchResult>, ApplicationError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if query.is_ascii() {
        return search_ascii_notes(connection, query);
    }

    // 非 ASCII 查询在 Rust 中做完整 Unicode 小写转换，避免 SQLite NOCASE 的 ASCII 限制。
    let folded_query = query.to_lowercase();
    let mut statement = connection
        .prepare(
            "SELECT id, derived_title, body, lifecycle
             FROM notes
             WHERE lifecycle IN ('active', 'archived')
             ORDER BY updated_at_ms DESC, id",
        )
        .map_err(|error| ApplicationError::storage("prepare_note_search", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| ApplicationError::storage("search_notes", error))?;
    let mut results = Vec::new();
    for row in rows {
        let (id, title, body, lifecycle) =
            row.map_err(|error| ApplicationError::storage("decode_search_result", error))?;
        let title_matches = title.to_lowercase().contains(&folded_query);
        let body_matches = body.to_lowercase().contains(&folded_query);
        if title_matches || body_matches {
            results.push(SearchResult {
                id: decode_uuid(id, "decode_search_note_id")?,
                title,
                lifecycle: decode_lifecycle(&lifecycle)?,
                matched_in_body: body_matches,
                exceeds_performance_guarantee: body.len() > PERFORMANCE_GUARANTEE_BODY_BYTES,
            });
        }
    }
    Ok(results)
}

fn search_ascii_notes(
    connection: &Connection,
    query: &str,
) -> Result<Vec<SearchResult>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT id, derived_title, lifecycle,
                    instr(lower(body), lower(?1)) > 0 AS body_matches,
                    length(CAST(body AS BLOB))
             FROM notes
             WHERE lifecycle IN ('active', 'archived')
               AND (instr(lower(derived_title), lower(?1)) > 0
                    OR instr(lower(body), lower(?1)) > 0)
             ORDER BY updated_at_ms DESC, id",
        )
        .map_err(|error| ApplicationError::storage("prepare_ascii_note_search", error))?;
    let rows = statement
        .query_map(params![query], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|error| ApplicationError::storage("search_ascii_notes", error))?;
    let mut results = Vec::new();
    for row in rows {
        let (id, title, lifecycle, matched_in_body, body_bytes) =
            row.map_err(|error| ApplicationError::storage("decode_ascii_search_result", error))?;
        results.push(SearchResult {
            id: decode_uuid(id, "decode_ascii_search_note_id")?,
            title,
            lifecycle: decode_lifecycle(&lifecycle)?,
            matched_in_body,
            exceeds_performance_guarantee: usize::try_from(body_bytes)
                .map(|bytes| bytes > PERFORMANCE_GUARANTEE_BODY_BYTES)
                .unwrap_or(true),
        });
    }
    Ok(results)
}

fn read_export_bundle(connection: &mut Connection) -> Result<ExportBundle, ApplicationError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|error| ApplicationError::storage("begin_export_snapshot", error))?;
    let current_note_bytes: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT note_id FROM current_note WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| ApplicationError::storage("export_current_note", error))?;
    let current_note_id = decode_optional_uuid(current_note_bytes, "decode_export_current")?;
    let notes = read_all_note_documents(&transaction)?;
    let reminders = read_export_reminders(&transaction)?;
    let shortcut: String =
        read_json_setting(&transaction, "global_shortcut", "export_global_shortcut")?;
    let startup_enabled: bool =
        read_json_setting(&transaction, "startup_enabled", "export_startup_enabled")?;
    let default_snooze_minutes: u16 = read_json_setting(
        &transaction,
        "default_snooze_minutes",
        "export_default_snooze",
    )?;
    let exported_at_ms = now_ms();
    transaction
        .commit()
        .map_err(|error| ApplicationError::storage("commit_export_snapshot", error))?;
    Ok(ExportBundle {
        format_version: 1,
        exported_at_ms,
        current_note_id,
        notes,
        reminders,
        settings: ExportSettings {
            global_shortcut: shortcut,
            startup_enabled,
            default_snooze_minutes,
        },
    })
}

fn read_all_note_documents(connection: &Connection) -> Result<Vec<NoteDocument>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT id, body, derived_title, lifecycle, content_revision,
                    created_at_ms, updated_at_ms, archived_at_ms, trashed_at_ms, due_at_ms
             FROM notes ORDER BY created_at_ms, id",
        )
        .map_err(|error| ApplicationError::storage("prepare_export_notes", error))?;
    let rows = statement
        .query_map([], decode_note_row)
        .map_err(|error| ApplicationError::storage("export_notes", error))?;
    rows.map(|row| row.map_err(|error| ApplicationError::storage("decode_export_note", error)))
        .collect()
}

fn read_export_reminders(connection: &Connection) -> Result<Vec<ExportReminder>, ApplicationError> {
    let mut statement = connection
        .prepare(
            "SELECT id, note_id, scheduled_at_ms, status, trigger_version
             FROM reminders ORDER BY created_at_ms, id",
        )
        .map_err(|error| ApplicationError::storage("prepare_export_reminders", error))?;
    let rows = statement
        .query_map([], |row| {
            let trigger_version: i64 = row.get(4)?;
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                trigger_version,
            ))
        })
        .map_err(|error| ApplicationError::storage("export_reminders", error))?;
    let mut reminders = Vec::new();
    for row in rows {
        let (id, note_id, scheduled_at_ms, status, trigger_version) =
            row.map_err(|error| ApplicationError::storage("decode_export_reminder", error))?;
        reminders.push(ExportReminder {
            id: decode_uuid(id, "decode_export_reminder_id")?,
            note_id: decode_uuid(note_id, "decode_export_reminder_note_id")?,
            scheduled_at_ms,
            status,
            trigger_version: trigger_version
                .try_into()
                .map_err(|error| ApplicationError::storage("decode_trigger_version", error))?,
        });
    }
    Ok(reminders)
}

fn decode_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteDocument> {
    let id = row.get::<_, Vec<u8>>(0)?;
    let lifecycle = row.get::<_, String>(3)?;
    let revision = row.get::<_, i64>(4)?;
    let id = Uuid::from_slice(&id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(16, rusqlite::types::Type::Blob, Box::new(error))
    })?;
    let lifecycle = decode_lifecycle_sql(&lifecycle)?;
    let content_revision = revision.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    Ok(NoteDocument {
        id,
        body: row.get(1)?,
        title: row.get(2)?,
        lifecycle,
        content_revision,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        archived_at_ms: row.get(7)?,
        trashed_at_ms: row.get(8)?,
        due_at_ms: row.get(9)?,
    })
}

fn decode_lifecycle(value: &str) -> Result<NoteLifecycle, ApplicationError> {
    match value {
        "active" => Ok(NoteLifecycle::Active),
        "archived" => Ok(NoteLifecycle::Archived),
        "trashed" => Ok(NoteLifecycle::Trashed),
        other => Err(ApplicationError::Storage {
            operation: "decode_note_lifecycle",
            message: format!("未知便签生命周期 {other}"),
        }),
    }
}

fn decode_lifecycle_sql(value: &str) -> rusqlite::Result<NoteLifecycle> {
    decode_lifecycle(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
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
    use super::{MAINTENANCE_INTERVAL, MaintenanceSchedule, derive_title};
    use std::time::{Duration, Instant};

    #[test]
    fn title_uses_first_non_empty_line_without_splitting_unicode() {
        assert_eq!(derive_title("  \n  第一行标题  \n第二行"), "第一行标题");
        assert_eq!(derive_title(" \n\t"), "");
        let long = "便".repeat(100);
        assert_eq!(derive_title(&long).chars().count(), 80);
    }

    #[test]
    fn storage_activity_does_not_reset_the_maintenance_deadline() {
        let started_at = Instant::now();
        let mut maintenance = MaintenanceSchedule::new(started_at);
        let request_at = started_at + Duration::from_secs(23 * 60 * 60);

        assert_eq!(
            maintenance.wait_from(request_at),
            Duration::from_secs(60 * 60)
        );
        assert!(!maintenance.is_due(request_at));

        let due_at = started_at + MAINTENANCE_INTERVAL;
        assert!(maintenance.is_due(due_at));
        maintenance.mark_completed(due_at);
        assert_eq!(maintenance.wait_from(due_at), MAINTENANCE_INTERVAL);
    }
}
