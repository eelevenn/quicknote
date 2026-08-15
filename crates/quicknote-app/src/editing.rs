//! 当前便签与空白草稿共享的有界自动保存协调器。

use crate::storage::{EditingView, SaveRequest, StorageClient, derive_title};
use crate::{ApplicationError, EditingSnapshot, EditorIntent, NoteSummary, SaveState};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

const TRAILING_DEBOUNCE: Duration = Duration::from_millis(250);
const MAXIMUM_WAIT: Duration = Duration::from_secs(1);

/// Application 唯一持有的编辑协调器；所有窗口共享同一份内存正文。
pub(crate) struct Editor {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    storage: StorageClient,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    note_id: Option<Uuid>,
    proposed_note_id: Option<Uuid>,
    body: String,
    title: String,
    revision: u64,
    saved_revision: u64,
    active_notes: Vec<NoteSummary>,
    in_flight_revision: Option<u64>,
    failure: Option<SaveFailure>,
    burst_started: Option<Instant>,
    last_edit: Option<Instant>,
    force_requested: bool,
    stopping: bool,
}

struct SaveFailure {
    revision: u64,
    error: ApplicationError,
}

impl Editor {
    /// 从持久化当前便签创建唯一编辑上下文并启动调度线程。
    pub(crate) fn start(storage: StorageClient) -> Result<Self, ApplicationError> {
        let initial = storage.load_editing_view()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::from_view(initial)),
            changed: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker_storage = storage.clone();
        let worker = thread::Builder::new()
            .name("quicknote-autosave".to_owned())
            .spawn(move || run_autosave(worker_shared, worker_storage))
            .map_err(|error| ApplicationError::WriterUnavailable {
                message: error.to_string(),
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            storage,
        })
    }

    /// 返回主页与快速记录共同读取的不可变编辑快照。
    pub(crate) fn snapshot(&self) -> Result<EditingSnapshot, ApplicationError> {
        let state = lock_state(&self.shared)?;
        Ok(snapshot_from_state(&state))
    }

    /// 执行一个高层编辑意图；保存排队和事务顺序留在模块内部。
    pub(crate) fn apply(&self, intent: EditorIntent) -> Result<EditingSnapshot, ApplicationError> {
        match intent {
            EditorIntent::ReplaceBody(body) => self.replace_body(body)?,
            EditorIntent::Flush => self.flush()?,
            EditorIntent::SwitchCurrent(note_id) => self.switch_current(note_id)?,
            EditorIntent::NewBlankDraft => self.new_blank_draft()?,
            EditorIntent::OpenCurrent => self.open_current()?,
            EditorIntent::RetrySave => self.retry_save()?,
        }
        self.snapshot()
    }

    fn replace_body(&self, body: String) -> Result<(), ApplicationError> {
        let mut state = lock_state(&self.shared)?;
        if state.body == body {
            return Ok(());
        }
        state.revision =
            state
                .revision
                .checked_add(1)
                .ok_or_else(|| ApplicationError::InvalidCommand {
                    message: "正文修订号已耗尽".to_owned(),
                })?;
        let was_clean = state.revision.saturating_sub(1) == state.saved_revision;
        state.body = body;
        state.title = derive_title(&state.body);
        state.failure = None;

        // 没有在途首次保存时，纯空白草稿直接视为无需持久化。
        if state.note_id.is_none()
            && state.in_flight_revision.is_none()
            && state.body.trim().is_empty()
        {
            state.saved_revision = state.revision;
            state.proposed_note_id = None;
            state.burst_started = None;
            state.last_edit = None;
            state.force_requested = false;
        } else {
            let now = Instant::now();
            if was_clean || state.burst_started.is_none() {
                state.burst_started = Some(now);
            }
            state.last_edit = Some(now);
        }
        self.shared.changed.notify_all();
        Ok(())
    }

    fn flush(&self) -> Result<(), ApplicationError> {
        loop {
            let mut state = lock_state(&self.shared)?;
            if state.revision == state.saved_revision {
                return Ok(());
            }

            // 显式刷新允许重试一次已经失败并留在内存中的修订。
            state.failure = None;
            state.force_requested = true;
            let target_revision = state.revision;
            self.shared.changed.notify_all();

            loop {
                if let Some(failure) = &state.failure {
                    return Err(failure.error.clone());
                }
                if state.saved_revision >= target_revision {
                    break;
                }
                if state.stopping {
                    return Err(ApplicationError::WriterUnavailable {
                        message: "自动保存协调器正在停止".to_owned(),
                    });
                }
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .map_err(|error| lock_error(error.to_string()))?;
            }

            // 若等待期间又有编辑，继续刷新更新后的最新修订。
            if state.revision == target_revision {
                return Ok(());
            }
        }
    }

    fn switch_current(&self, note_id: Uuid) -> Result<(), ApplicationError> {
        self.flush()?;
        let view = self.storage.switch_current(note_id)?;
        let mut state = lock_state(&self.shared)?;
        state.replace_with_view(view);
        self.shared.changed.notify_all();
        Ok(())
    }

    fn new_blank_draft(&self) -> Result<(), ApplicationError> {
        self.flush()?;
        let mut state = lock_state(&self.shared)?;
        let active_notes = state.active_notes.clone();
        *state = State::blank(active_notes);
        self.shared.changed.notify_all();
        Ok(())
    }

    fn open_current(&self) -> Result<(), ApplicationError> {
        self.flush()?;
        let view = self.storage.load_editing_view()?;
        let mut state = lock_state(&self.shared)?;
        state.replace_with_view(view);
        self.shared.changed.notify_all();
        Ok(())
    }

    fn retry_save(&self) -> Result<(), ApplicationError> {
        let mut state = lock_state(&self.shared)?;
        if state.revision != state.saved_revision {
            state.failure = None;
            state.force_requested = true;
            self.shared.changed.notify_all();
        }
        Ok(())
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            self.shared.changed.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl State {
    fn from_view(view: EditingView) -> Self {
        Self {
            note_id: view.document.note_id,
            proposed_note_id: None,
            body: view.document.body,
            title: view.document.title,
            revision: view.document.revision,
            saved_revision: view.document.revision,
            active_notes: view.active_notes,
            in_flight_revision: None,
            failure: None,
            burst_started: None,
            last_edit: None,
            force_requested: false,
            stopping: false,
        }
    }

    fn blank(active_notes: Vec<NoteSummary>) -> Self {
        Self {
            note_id: None,
            proposed_note_id: None,
            body: String::new(),
            title: String::new(),
            revision: 0,
            saved_revision: 0,
            active_notes,
            in_flight_revision: None,
            failure: None,
            burst_started: None,
            last_edit: None,
            force_requested: false,
            stopping: false,
        }
    }

    fn replace_with_view(&mut self, view: EditingView) {
        *self = Self::from_view(view);
    }

    fn due_at(&self) -> Option<Instant> {
        let trailing = self.last_edit?.checked_add(TRAILING_DEBOUNCE)?;
        let maximum = self.burst_started?.checked_add(MAXIMUM_WAIT)?;
        Some(trailing.min(maximum))
    }
}

fn run_autosave(shared: Arc<Shared>, storage: StorageClient) {
    loop {
        let (request, request_revision) = {
            let mut state = match shared.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            loop {
                if state.stopping {
                    return;
                }
                if state.failure.is_some() || state.revision == state.saved_revision {
                    state = match shared.changed.wait(state) {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    continue;
                }

                if !state.force_requested {
                    let due_at = state.due_at().unwrap_or_else(Instant::now);
                    let now = Instant::now();
                    if due_at > now {
                        let wait = due_at.saturating_duration_since(now);
                        let waited = match shared.changed.wait_timeout(state, wait) {
                            Ok(waited) => waited,
                            Err(_) => return,
                        };
                        state = waited.0;
                        if !waited.1.timed_out() {
                            continue;
                        }
                    }
                }

                let candidate_note_id = *state.proposed_note_id.get_or_insert_with(Uuid::now_v7);
                let request_revision = state.revision;
                let request = SaveRequest {
                    persisted_note_id: state.note_id,
                    candidate_note_id,
                    body: state.body.clone(),
                    revision: request_revision,
                };
                state.in_flight_revision = Some(request_revision);
                state.force_requested = false;
                break (request, request_revision);
            }
        };

        let result = storage.save(request);
        let mut state = match shared.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        state.in_flight_revision = None;
        match result {
            Ok(view) => {
                state.note_id = view.document.note_id;
                state.proposed_note_id = None;
                state.saved_revision = request_revision;
                state.active_notes = view.active_notes;
                state.failure = None;
                if state.revision == request_revision {
                    state.burst_started = None;
                    state.last_edit = None;
                }
            }
            Err(error) => {
                state.failure = Some(SaveFailure {
                    revision: request_revision,
                    error,
                });
            }
        }
        shared.changed.notify_all();
    }
}

fn snapshot_from_state(state: &State) -> EditingSnapshot {
    let save_state = if state.note_id.is_none()
        && state.in_flight_revision.is_none()
        && state.body.trim().is_empty()
    {
        SaveState::BlankDraft
    } else if let Some(failure) = &state.failure {
        SaveState::Failed {
            message: format!("修订 {}：{}", failure.revision, failure.error),
        }
    } else if state.in_flight_revision.is_some() {
        SaveState::Saving
    } else if state.revision == state.saved_revision {
        SaveState::Saved
    } else {
        SaveState::Scheduled
    };

    EditingSnapshot {
        note_id: state.note_id,
        body: state.body.clone(),
        title: state.title.clone(),
        revision: state.revision,
        saved_revision: state.saved_revision,
        save_state,
        active_notes: state.active_notes.clone(),
    }
}

fn lock_state(shared: &Shared) -> Result<MutexGuard<'_, State>, ApplicationError> {
    shared
        .state
        .lock()
        .map_err(|error| lock_error(error.to_string()))
}

fn lock_error(message: String) -> ApplicationError {
    ApplicationError::WriterUnavailable {
        message: format!("自动保存状态不可用：{message}"),
    }
}
